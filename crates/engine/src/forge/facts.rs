use super::*;

/// Exact authority resolution of one forge coordinate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForgeResolution {
    /// The requested typed ref.
    pub requested: ForgeRevision,
    /// The exact commit object selected by the authority.
    pub commit: ForgeObjectId,
    /// The exact Git tree object selected by the authority, when reported.
    pub tree: Option<ForgeObjectId>,
    /// Canonical repository authority that made this resolution, when the
    /// transport supplied one.  A bound resolution cannot be replayed for a
    /// different host or repository.
    #[serde(default)]
    pub authority: Option<Arc<str>>,
    /// Validator observed with the ref lookup (usually an ETag or the exact
    /// commit spelling).  Mutable refs must carry this binding so a tag or
    /// branch is never silently treated as an immutable version.
    #[serde(default)]
    pub validator: Option<Arc<str>>,
}

impl ForgeResolution {
    /// Admits a resolution and checks an immutable commit request exactly.
    pub fn new(
        requested: ForgeRevision,
        commit: ForgeObjectId,
        tree: Option<ForgeObjectId>,
    ) -> Result<Self, ForgeProtocolError> {
        if let ForgeRevision::Commit(expected) = &requested
            && expected != &commit
        {
            return Err(ForgeProtocolError::RevisionMismatch);
        }
        Ok(Self {
            requested,
            commit,
            tree,
            authority: None,
            validator: None,
        })
    }

    /// Creates a resolution explicitly bound to one coordinate authority and
    /// the validator observed while resolving its ref.
    pub fn for_coordinate(
        coordinate: &ForgeCoordinate,
        commit: ForgeObjectId,
        tree: Option<ForgeObjectId>,
        validator: impl Into<String>,
    ) -> Result<Self, ForgeProtocolError> {
        let mut resolution = Self::new(coordinate.revision().clone(), commit, tree)?;
        resolution.authority = Some(Arc::from(coordinate.repository_url()));
        let validator = validator.into();
        if validator.is_empty()
            || validator
                .bytes()
                .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
        {
            return Err(ForgeProtocolError::Malformed);
        }
        resolution.validator = Some(Arc::from(validator));
        Ok(resolution)
    }

    /// Checks the authority and validator binding supplied by a transport.
    pub fn validate_for(&self, coordinate: &ForgeCoordinate) -> Result<(), ForgeProtocolError> {
        if self.requested != coordinate.revision().clone() {
            return Err(ForgeProtocolError::RevisionMismatch);
        }
        if self
            .authority
            .as_deref()
            .is_some_and(|authority| authority != coordinate.repository_url())
        {
            return Err(ForgeProtocolError::AuthorityMismatch);
        }
        if matches!(
            coordinate.revision(),
            ForgeRevision::Tag(_) | ForgeRevision::Branch(_)
        ) && self.validator.is_none()
        {
            return Err(ForgeProtocolError::ValidatorRequired);
        }
        Ok(())
    }

    /// Returns the stable resolved revision identity.
    #[must_use]
    pub fn identity(&self) -> [u8; ID_BYTES] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.forge.resolution.v1\0");
        if let Some(authority) = &self.authority {
            hasher.update(b"authority\0");
            hasher.update(authority.as_bytes());
        }
        if let Some(validator) = &self.validator {
            hasher.update(b"validator\0");
            hasher.update(validator.as_bytes());
        }
        hasher.update(&self.requested.canonical_bytes());
        hasher.update(self.commit.as_hex().as_bytes());
        if let Some(tree) = &self.tree {
            hasher.update(tree.as_hex().as_bytes());
        }
        *hasher.finalize().as_bytes()
    }
}

/// Typed metadata availability retained from the forge authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ForgeFact<T> {
    /// The authority reported the value.
    Recorded(T),
    /// The authority did not provide the value or it was unavailable.
    Unavailable(ForgeUnavailableReason),
}

/// Why one forge metadata value is unavailable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeUnavailableReason {
    /// The selected authority omits this field.
    AuthorityOmitted,
    /// The metadata endpoint could not be reached.
    Unreachable,
    /// The authority does not implement this operation.
    Unsupported,
    /// Local policy is offline.
    Offline,
    /// A configured bound rejected the response.
    Bounds,
    /// The response was malformed.
    Malformed,
}

impl ForgeUnavailableReason {
    /// Stable product-facing reason spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityOmitted => "authority omitted",
            Self::Unreachable => "authority unreachable",
            Self::Unsupported => "authority unsupported",
            Self::Offline => "offline",
            Self::Bounds => "response exceeded bounds",
            Self::Malformed => "authority response malformed",
        }
    }
}

/// Repository metadata returned by a forge authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeRepositoryMetadata {
    /// Owner/group as recorded by the authority.
    pub owner: ForgeFact<ProductText>,
    /// Human description, when reported.
    pub description: ForgeFact<ProductText>,
    /// License identifier, when reported.
    pub license: ForgeFact<ProductText>,
    /// README content, bounded and retained as recorded text.
    pub readme: ForgeFact<ProductText>,
    /// Topics/tags, when the authority reports them.
    pub topics: ForgeFact<Box<[ProductText]>>,
    /// Repository stars, only when a numeric field was present.
    pub stars: ForgeFact<u64>,
    /// Repository forks, only when a numeric field was present.
    pub forks: ForgeFact<u64>,
}

impl ForgeRepositoryMetadata {
    /// Creates the typed unavailable baseline for a repository.
    #[must_use]
    pub fn unavailable(owner: &str, reason: ForgeUnavailableReason) -> Self {
        let owner = ProductText::new(owner)
            .map(ForgeFact::Recorded)
            .unwrap_or(ForgeFact::Unavailable(ForgeUnavailableReason::Malformed));
        Self {
            owner,
            description: ForgeFact::Unavailable(reason),
            license: ForgeFact::Unavailable(reason),
            readme: ForgeFact::Unavailable(reason),
            topics: ForgeFact::Unavailable(reason),
            stars: ForgeFact::Unavailable(reason),
            forks: ForgeFact::Unavailable(reason),
        }
    }

    fn from_json(owner: &str, value: &Value) -> Self {
        let recorded_text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .and_then(|value| ProductText::new(value).ok())
                .map_or(
                    ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                    ForgeFact::Recorded,
                )
        };
        let recorded_number = |value: Option<&Value>| {
            value.map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                |value| {
                    value.as_u64().map_or(
                        ForgeFact::Unavailable(ForgeUnavailableReason::Malformed),
                        ForgeFact::Recorded,
                    )
                },
            )
        };
        let topics = value
            .get("topics")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|value| ProductText::new(value).ok())
                    .take(256)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                ForgeFact::Recorded,
            );
        Self {
            owner: ProductText::new(
                value
                    .get("owner")
                    .and_then(|owner| owner.get("login").or_else(|| owner.get("username")))
                    .and_then(Value::as_str)
                    .unwrap_or(owner),
            )
            .map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::Malformed),
                ForgeFact::Recorded,
            ),
            description: recorded_text(value.get("description")),
            license: recorded_text(
                value
                    .get("license")
                    .and_then(|license| license.get("spdx_id").or_else(|| license.get("name")))
                    .or_else(|| value.get("license_name")),
            ),
            readme: ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
            topics,
            stars: recorded_number(value.get("stargazers_count").or_else(|| value.get("stars"))),
            forks: recorded_number(value.get("forks_count").or_else(|| value.get("forks"))),
        }
    }
}
