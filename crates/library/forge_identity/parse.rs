//! Parsing and construction helpers for [`ForgeCoordinate`].
use super::*;

impl ForgeCoordinate {
    /// Parses an HTTPS repository URL whose ref is supplied as a fragment or
    /// `@tag:...`/`@branch:...`/`@commit:...` suffix.
    pub fn parse(value: impl Into<String>) -> Result<Self, ForgeCoordinateError> {
        let value = value.into();
        if value.len() > MAX_COORDINATE_BYTES {
            return Err(ForgeCoordinateError::Bounds);
        }
        let (url, revision, subdir) = split_coordinate_suffix(&value)?;
        let (provider, base_url, owner, repository) = parse_repository_url(&url)?;
        let revision = revision.ok_or(ForgeCoordinateError::RevisionRequired)?;
        Self::new_parts(provider, base_url, owner, repository, revision, subdir)
    }

    /// Constructs a coordinate from a repository URL and an explicit revision.
    pub fn new(
        repository_url: impl Into<String>,
        revision: ForgeRevision,
        subdir: Option<impl Into<String>>,
    ) -> Result<Self, ForgeCoordinateError> {
        let (provider, base_url, owner, repository) = parse_repository_url(&repository_url.into())?;
        Self::new_parts(
            provider,
            base_url,
            owner,
            repository,
            revision,
            subdir.map(|value| value.into()),
        )
    }

    fn new_parts(
        provider: ForgeProvider,
        base_url: Arc<str>,
        owner: Arc<str>,
        repository: Arc<str>,
        revision: ForgeRevision,
        subdir: Option<String>,
    ) -> Result<Self, ForgeCoordinateError> {
        let subdir = subdir
            .map(|value| path_part(&value, MAX_COORDINATE_BYTES))
            .transpose()?;
        let mut coordinate = Self {
            provider,
            base_url,
            owner,
            repository,
            revision,
            subdir,
            identity: [0; ID_BYTES],
        };
        coordinate.identity = coordinate.derive_identity();
        Ok(coordinate)
    }

    fn derive_identity(&self) -> [u8; ID_BYTES] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.forge.coordinate.v1\0");
        hasher.update(self.provider.as_str().as_bytes());
        hasher.update(self.base_url.as_bytes());
        hasher.update(self.owner.as_bytes());
        hasher.update(self.repository.as_bytes());
        hasher.update(&self.revision.canonical_bytes());
        if let Some(subdir) = &self.subdir {
            hasher.update(b"subdir\0");
            hasher.update(subdir.as_bytes());
        }
        *hasher.finalize().as_bytes()
    }

    /// Verifies that a deserialized coordinate still matches its identity.
    #[must_use]
    pub fn identity_is_valid(&self) -> bool {
        let Ok((provider, base_url, owner, repository)) = parse_repository_url(&self.base_url)
        else {
            return false;
        };
        let subdir_valid = match &self.subdir {
            Some(subdir) => path_part(subdir, MAX_COORDINATE_BYTES).is_ok(),
            None => true,
        };
        self.provider == provider
            && self.base_url.as_ref() == base_url.as_ref()
            && self.owner.as_ref() == owner.as_ref()
            && self.repository.as_ref() == repository.as_ref()
            && self.revision.is_valid()
            && subdir_valid
            && self.identity == self.derive_identity()
    }

    /// Returns the provider selected from the repository host.
    #[must_use]
    pub const fn provider(&self) -> ForgeProvider {
        self.provider
    }

    /// Returns the credential-free canonical repository URL.
    #[must_use]
    pub fn repository_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the canonical owner/group path.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Returns the canonical repository name without `.git`.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the exact requested revision.
    #[must_use]
    pub const fn revision(&self) -> &ForgeRevision {
        &self.revision
    }

    /// Returns the optional package-manifest subdirectory.
    #[must_use]
    pub fn subdir(&self) -> Option<&str> {
        self.subdir.as_deref()
    }

    /// Returns the credential-free coordinate identity.
    #[must_use]
    pub const fn identity(&self) -> [u8; ID_BYTES] {
        self.identity
    }

    /// Returns the canonical coordinate spelling used in journals and DTOs.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut value = self.base_url.to_string();
        value.push('@');
        value.push_str(&String::from_utf8_lossy(&self.revision.canonical_bytes()));
        if let Some(subdir) = &self.subdir {
            value.push('#');
            value.push_str(subdir);
        }
        value
    }

    /// Returns a coordinate with a different explicit revision.
    pub fn with_revision(&self, revision: ForgeRevision) -> Result<Self, ForgeCoordinateError> {
        Self::new_parts(
            self.provider,
            Arc::clone(&self.base_url),
            Arc::clone(&self.owner),
            Arc::clone(&self.repository),
            revision,
            self.subdir.as_deref().map(ToOwned::to_owned),
        )
    }
}
