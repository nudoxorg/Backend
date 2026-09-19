use std::sync::Arc;

/// A checked relation transition bound to exact workspace roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationTransition {
    schema: SchemaIdentity,
    base: [u8; ID_BYTES],
    target: [u8; ID_BYTES],
    delta: [u8; ID_BYTES],
    canonical_changes: Arc<[u8]>,
    checked: bool,
}

/// Alias naming a relation transition before typed-delta admission.
pub type UntrustedRelationTransition = RelationTransition;

impl RelationTransition {
    /// Erases one already checked typed relation delta for a workspace change.
    #[must_use]
    pub fn from_delta<R: Relation>(delta: &Delta<R>) -> Self {
        Self {
            schema: SchemaIdentity::of_relation::<R>(),
            base: delta.base().to_bytes(),
            target: delta.target().to_bytes(),
            delta: delta.id().to_bytes(),
            canonical_changes: delta.canonical_changes_arc(),
            checked: true,
        }
    }

    /// Revalidates the typed delta before it crosses the erased workspace
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::InvalidTransition`] if the delta's private
    /// change sequence no longer derives its claimed identity.
    pub fn try_from_delta<R: Relation>(delta: &Delta<R>) -> Result<Self, WorkspaceError> {
        if !delta.is_canonical() {
            return Err(WorkspaceError::InvalidTransition);
        }
        Ok(Self::from_delta(delta))
    }

    /// Encodes this transition in the canonical bounded workspace envelope.
    ///
    /// The checked marker is omitted.  A receiver must admit the descriptor
    /// against the exact typed [`Delta`] before it can cross into a checked
    /// workspace transition.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&TRANSITION_WIRE_MAGIC);
        out.push(WORKSPACE_WIRE_VERSION);
        push_schema(&mut out, self.schema);
        push_digest(&mut out, self.base);
        push_digest(&mut out, self.target);
        push_digest(&mut out, self.delta);
        append_wire_field(&mut out, &self.canonical_changes);
        out
    }

    /// Decodes a relation transition while retaining an unverified marker.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded envelope is malformed, oversized,
    /// uses another wire version, or contains trailing bytes.
    pub fn decode_untrusted(bytes: &[u8]) -> Result<Self, WorkspaceDecodeError> {
        let mut reader = WireReader::new(bytes)?;
        reader.magic(TRANSITION_WIRE_MAGIC)?;
        let schema = read_schema(&mut reader)?;
        let base = reader.digest()?;
        let target = reader.digest()?;
        let delta = reader.digest()?;
        let canonical_changes: Arc<[u8]> =
            Arc::<[u8]>::from(reader.field()?.to_vec().into_boxed_slice());
        if canonical_changes.len() > MAX_WORKSPACE_WIRE_BYTES {
            return Err(WorkspaceDecodeError::InvalidLength);
        }
        reader.finish()?;
        Ok(Self {
            schema,
            base,
            target,
            delta,
            canonical_changes,
            checked: false,
        })
    }

    /// Admits this erased transition against one exact typed relation delta.
    ///
    /// The relation schema, base and target roots, change bytes, and delta ID
    /// are all compared.  Context metadata or copied roots alone cannot mint
    /// the checked transition marker.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::TransitionMismatch`] or
    /// [`WorkspaceError::InvalidTransition`] when any exact field differs.
    pub fn admit_delta<R: Relation>(&self, delta: &Delta<R>) -> Result<Self, WorkspaceError> {
        if !delta.is_canonical() {
            return Err(WorkspaceError::InvalidTransition);
        }
        if self.schema != SchemaIdentity::of_relation::<R>()
            || self.base != delta.base().to_bytes()
            || self.target != delta.target().to_bytes()
            || self.delta != delta.id().to_bytes()
            || self.canonical_changes.as_ref() != delta.canonical_changes_bytes()
        {
            return Err(WorkspaceError::TransitionMismatch);
        }
        Ok(Self {
            checked: true,
            ..self.clone()
        })
    }

    /// Alias for [`Self::admit_delta`] emphasizing the wire-boundary use.
    ///
    /// # Errors
    ///
    /// Returns the same exact-transition errors as [`Self::admit_delta`].
    pub fn admit_against<R: Relation>(&self, delta: &Delta<R>) -> Result<Self, WorkspaceError> {
        self.admit_delta(delta)
    }

    /// Short alias for [`Self::admit_delta`].
    ///
    /// # Errors
    ///
    /// Returns the same exact-transition errors as [`Self::admit_delta`].
    pub fn admit<R: Relation>(&self, delta: &Delta<R>) -> Result<Self, WorkspaceError> {
        self.admit_delta(delta)
    }

    /// Returns the relation schema tag.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the exact base relation root.
    #[must_use]
    pub const fn base(&self) -> [u8; ID_BYTES] {
        self.base
    }

    /// Returns the exact target relation root.
    #[must_use]
    pub const fn target(&self) -> [u8; ID_BYTES] {
        self.target
    }

    /// Returns the bound delta identity.
    #[must_use]
    pub const fn delta(&self) -> [u8; ID_BYTES] {
        self.delta
    }

    /// Borrows the cached canonical change encoding without allocating.
    #[must_use]
    pub fn canonical_changes_bytes(&self) -> &[u8] {
        &self.canonical_changes
    }

    /// Returns whether this transition was admitted against a typed delta.
    #[must_use]
    pub const fn is_checked(&self) -> bool {
        self.checked
    }

    fn is_canonical(&self) -> bool {
        if !self.checked {
            return false;
        }
        let expected = digest(
            0x44,
            self.schema.domain(),
            self.schema.ty(),
            self.schema.version(),
            &[&self.base, &self.target, &self.canonical_changes],
        );
        expected == self.delta
    }
}
