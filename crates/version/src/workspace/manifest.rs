/// One selected relation root in a workspace manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationBinding {
    schema: SchemaIdentity,
    root: [u8; ID_BYTES],
    checked: bool,
    complete: bool,
}

impl RelationBinding {
    /// Creates a relation binding; ordering is checked by the manifest.
    #[must_use]
    pub const fn new(schema: SchemaIdentity, root: [u8; ID_BYTES]) -> Self {
        Self {
            schema,
            root,
            checked: false,
            complete: false,
        }
    }

    /// Binds a relation to a canonical typed state root.
    ///
    /// The state object has already checked key ordering, node limits, and its
    /// root commitment.  Only this constructor marks the binding as eligible
    /// for an admitted workspace closure.
    #[must_use]
    pub fn from_state<R: Relation>(state: &RelationState<R>) -> Self {
        Self {
            schema: SchemaIdentity::of_relation::<R>(),
            root: state.root().to_bytes(),
            checked: true,
            complete: state.coverage().state().is_complete() && state.coverage().is_bound(),
        }
    }

    /// Binds an admitted persisted relation root without materializing rows.
    ///
    /// The root handle supplies canonical node admission and the coverage
    /// witness supplies the authority needed to make the relation complete.
    /// Incomplete coverage remains represented but is rejected by checked
    /// manifest admission.
    #[must_use]
    pub fn from_persisted_root<R: CanonicalRelation>(
        root: &PersistedTreeRoot<R>,
        coverage: CoverageWitness,
    ) -> Self {
        Self {
            schema: root.schema(),
            root: root.root().to_bytes(),
            checked: true,
            complete: coverage.state().is_complete() && coverage.is_bound(),
        }
    }

    /// Returns the relation schema tag.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the selected relation root.
    #[must_use]
    pub const fn root(self) -> [u8; ID_BYTES] {
        self.root
    }

    /// Returns whether this binding carries a typed relation-state proof.
    #[must_use]
    pub const fn is_checked(self) -> bool {
        self.checked
    }

    /// Returns whether the typed state also carries complete authority
    /// coverage.  A workspace commit requires this stronger closure.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.complete
    }
}

/// One input or basis root contributing to workspace identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BasisBinding {
    schema: SchemaIdentity,
    root: [u8; ID_BYTES],
    checked: bool,
}

impl BasisBinding {
    /// Creates a basis binding; ordering is checked by the manifest.
    #[must_use]
    pub const fn new(schema: SchemaIdentity, root: [u8; ID_BYTES]) -> Self {
        Self {
            schema,
            root,
            checked: false,
        }
    }

    /// Binds a basis object to its canonical value identity.
    #[must_use]
    pub fn from_version<T: Schema>(version: ObjectVersion<T>) -> Self {
        Self {
            schema: SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION),
            root: version.to_bytes(),
            checked: true,
        }
    }

    /// Returns the basis schema tag.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the basis root.
    #[must_use]
    pub const fn root(self) -> [u8; ID_BYTES] {
        self.root
    }

    /// Returns whether this binding came from a checked typed object version.
    #[must_use]
    pub const fn is_checked(self) -> bool {
        self.checked
    }
}

/// A typed object closure used for workspace authority and commit provenance.
///
/// The schema and version bytes are captured from an admitted
/// [`ObjectVersion`]. Its private marker prevents a caller from replacing a
/// raw `[u8; 32]` root while retaining checked status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectClosure {
    schema: SchemaIdentity,
    root: [u8; ID_BYTES],
    _proof: ClosureProof,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ClosureProof;

impl ObjectClosure {
    /// Captures an exact typed object version as a workspace closure input.
    #[must_use]
    pub fn from_version<T: Schema>(version: ObjectVersion<T>) -> Self {
        Self {
            schema: SchemaIdentity::new(T::DOMAIN, T::TYPE, T::VERSION),
            root: version.to_bytes(),
            _proof: ClosureProof,
        }
    }

    /// Returns the schema identity carried by the closure.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the canonical object version bytes.
    #[must_use]
    pub const fn root(self) -> [u8; ID_BYTES] {
        self.root
    }
}

/// Kind of durable object selected by a checked workspace closure.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ClosureKind {
    /// A relation's canonical state object.
    Relation,
    /// A basis/input object.
    Basis,
    /// The authority object selected by the manifest.
    Authority,
    /// A transaction object selected by commit provenance.
    Transaction,
}

/// Opaque typed schema/version reference that a store must make durable.
///
/// Values are emitted only by checked manifests or checked provenance.  The
/// public accessors expose the lookup key needed by a store while the private
/// marker prevents callers from manufacturing a checked closure list from raw
/// digest bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ClosureRef {
    kind: ClosureKind,
    schema: SchemaIdentity,
    version: [u8; ID_BYTES],
    _proof: ClosureProof,
}

impl ClosureRef {
    fn new(kind: ClosureKind, schema: SchemaIdentity, version: [u8; ID_BYTES]) -> Self {
        Self {
            kind,
            schema,
            version,
            _proof: ClosureProof,
        }
    }

    /// Returns the selected durable-object kind.
    #[must_use]
    pub const fn kind(self) -> ClosureKind {
        self.kind
    }

    /// Returns the selected object's schema identity.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the exact content-addressed object-version bytes.
    #[must_use]
    pub const fn version(self) -> [u8; ID_BYTES] {
        self.version
    }
}

/// Marker for a manifest descriptor that has not crossed closure admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedManifest;

/// Marker for a manifest whose closure and schema have been admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedManifest;

/// Atomic root-of-roots for authoritative workspace state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceManifest<S = CheckedManifest> {
    schema: u32,
    relations: Vec<RelationBinding>,
    basis: Vec<BasisBinding>,
    authority: [u8; ID_BYTES],
    authority_schema: Option<SchemaIdentity>,
    coverage: CoverageWitness,
    checked: bool,
    _state: core::marker::PhantomData<fn() -> S>,
}

/// Alias naming the unchecked form returned by
/// [`WorkspaceManifest::decode_untrusted`].
///
/// ```compile_fail
/// use backend_version::UntrustedWorkspaceManifest;
/// fn forged_root(manifest: &UntrustedWorkspaceManifest) {
///     let _ = manifest.root();
/// }
/// ```
pub type UntrustedWorkspaceManifest = WorkspaceManifest<UntrustedManifest>;

/// Alias naming a manifest whose closure has been admitted.
pub type CheckedWorkspaceManifest = WorkspaceManifest<CheckedManifest>;

impl WorkspaceManifest<UntrustedManifest> {
    /// Constructs a canonical but untrusted manifest descriptor.
    ///
    /// # Errors
    ///
    /// Returns an ordering error for duplicate or unsorted bindings.
    pub fn new(
        schema: u32,
        relations: Vec<RelationBinding>,
        basis: Vec<BasisBinding>,
        authority: [u8; ID_BYTES],
        coverage: CoverageWitness,
    ) -> Result<Self, WorkspaceError> {
        if relations
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceError::NonCanonicalRelations);
        }
        if basis
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceError::NonCanonicalBasis);
        }
        Ok(Self {
            schema,
            relations,
            basis,
            authority,
            authority_schema: None,
            coverage,
            checked: false,
            _state: core::marker::PhantomData,
        })
    }

    /// Decodes a bounded manifest without granting closure authority.
    ///
    /// # Errors
    ///
    /// Returns a wire, schema, or canonical-order error for malformed input.
    pub fn decode_untrusted(bytes: &[u8]) -> Result<Self, WorkspaceDecodeError> {
        let mut reader = WireReader::new(bytes)?;
        reader.magic(MANIFEST_WIRE_MAGIC)?;
        let schema = reader.u32()?;
        if schema != u32::from(CANONICAL_VERSION) {
            return Err(WorkspaceDecodeError::UnsupportedVersion);
        }
        let coverage = decode_coverage(&mut reader)?;
        let authority_schema = match reader.byte()? {
            0 => None,
            1 => Some(read_schema(&mut reader)?),
            _ => return Err(WorkspaceDecodeError::InvalidTag),
        };
        let authority = reader.digest()?;
        let relation_count = reader.count()?;
        let mut relations = Vec::with_capacity(relation_count);
        for _ in 0..relation_count {
            relations.push(RelationBinding::new(
                read_schema(&mut reader)?,
                reader.digest()?,
            ));
        }
        let basis_count = reader.count()?;
        let mut basis = Vec::with_capacity(basis_count);
        for _ in 0..basis_count {
            basis.push(BasisBinding::new(
                read_schema(&mut reader)?,
                reader.digest()?,
            ));
        }
        reader.finish()?;
        let mut manifest = Self::new(schema, relations, basis, authority, coverage)
            .map_err(WorkspaceDecodeError::Semantic)?;
        manifest.authority_schema = authority_schema;
        Ok(manifest)
    }

    /// Rebinds this descriptor to exact typed closures and admitted coverage.
    ///
    /// # Errors
    ///
    /// Returns a closure, schema, root, coverage, or ordering error when any
    /// supplied typed input differs from this descriptor.
    pub fn admit_checked(
        self,
        relations: Vec<RelationBinding>,
        basis: Vec<BasisBinding>,
        authority: ObjectClosure,
        coverage: CoverageWitness,
    ) -> Result<CheckedWorkspaceManifest, WorkspaceError> {
        self.validate_ordering()?;
        if !same_relation_bindings(&self.relations, &relations) {
            return Err(WorkspaceError::UnverifiedRelation);
        }
        if !same_basis_bindings(&self.basis, &basis) {
            return Err(WorkspaceError::UnverifiedBasis);
        }
        match self.authority_schema {
            Some(schema) if schema == authority.schema => {}
            Some(_) => return Err(WorkspaceError::AuthorityMismatch),
            None => return Err(WorkspaceError::UnverifiedAuthority),
        }
        if self.authority != authority.root {
            return Err(WorkspaceError::AuthorityMismatch);
        }
        if !self.coverage.same_identity(coverage) {
            return Err(WorkspaceError::CoverageMismatch);
        }
        WorkspaceManifest::<CheckedManifest>::new_checked(
            self.schema,
            relations,
            basis,
            authority,
            coverage,
        )
    }

    /// Alias for [`Self::admit_checked`] for transport adapters.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::admit_checked`].
    pub fn admit(
        self,
        relations: Vec<RelationBinding>,
        basis: Vec<BasisBinding>,
        authority: ObjectClosure,
        coverage: CoverageWitness,
    ) -> Result<CheckedWorkspaceManifest, WorkspaceError> {
        self.admit_checked(relations, basis, authority, coverage)
    }
}

impl WorkspaceManifest<CheckedManifest> {
    /// Constructs an admitted workspace manifest from typed closures.
    ///
    /// # Errors
    ///
    /// Returns a schema, ordering, closure, or coverage error.
    pub fn new_checked(
        schema: u32,
        relations: Vec<RelationBinding>,
        basis: Vec<BasisBinding>,
        authority: ObjectClosure,
        coverage: CoverageWitness,
    ) -> Result<Self, WorkspaceError> {
        if schema != u32::from(CANONICAL_VERSION) {
            return Err(WorkspaceError::SchemaMismatch);
        }
        let manifest = Self {
            schema,
            relations,
            basis,
            authority: authority.root,
            authority_schema: Some(authority.schema),
            coverage,
            checked: true,
            _state: core::marker::PhantomData,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Constructs an admitted workspace manifest from a typed authority.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::new_checked`].
    pub fn from_versions<T: Schema>(
        schema: u32,
        relations: Vec<RelationBinding>,
        basis: Vec<BasisBinding>,
        authority: ObjectVersion<T>,
        coverage: CoverageWitness,
    ) -> Result<Self, WorkspaceError> {
        Self::new_checked(
            schema,
            relations,
            basis,
            ObjectClosure::from_version(authority),
            coverage,
        )
    }

    /// Computes the canonical workspace root including all admitted inputs.
    #[must_use]
    pub fn root(&self) -> WorkspaceRoot {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.schema.to_be_bytes());
        bytes.push(self.coverage.state() as u8);
        bytes.extend_from_slice(self.coverage.scope_root().as_bytes());
        if let Some(identity) = self.coverage.complete_identity() {
            identity.append_observation_identity(&mut bytes);
        }
        append_field(&mut bytes, |out| {
            if let Some(schema) = self.authority_schema {
                out.push(1);
                out.push(schema.domain());
                out.extend_from_slice(&schema.ty().to_be_bytes());
                out.push(schema.version());
            } else {
                out.push(0);
            }
            out.extend_from_slice(&self.authority);
        });
        append_bindings(&mut bytes, &self.relations);
        append_field(&mut bytes, |out| {
            for binding in &self.basis {
                append_binding(out, binding.schema, binding.root);
            }
        });
        workspace_root_from_digest(digest(0x57, 0, 0, CANONICAL_VERSION, &[&bytes]))
    }
}

impl<S> WorkspaceManifest<S> {
    /// Encodes this manifest in the canonical bounded workspace envelope.
    ///
    /// Checked markers are intentionally omitted from the wire format.  A
    /// decoder must obtain fresh typed relation, basis, authority, and
    /// coverage evidence before the descriptor can be used for a transition
    /// or commit.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MANIFEST_WIRE_MAGIC);
        out.push(WORKSPACE_WIRE_VERSION);
        out.extend_from_slice(&self.schema.to_be_bytes());
        out.push(coverage_tag(self.coverage));
        push_digest(&mut out, *self.coverage.scope_root().as_bytes());
        if let Some(identity) = self.coverage.complete_identity() {
            identity.append_observation_identity(&mut out);
        }
        match self.authority_schema {
            Some(schema) => {
                out.push(1);
                push_schema(&mut out, schema);
            }
            None => out.push(0),
        }
        push_digest(&mut out, self.authority);
        push_count(&mut out, self.relations.len());
        for binding in &self.relations {
            push_schema(&mut out, binding.schema);
            push_digest(&mut out, binding.root);
        }
        push_count(&mut out, self.basis.len());
        for binding in &self.basis {
            push_schema(&mut out, binding.schema);
            push_digest(&mut out, binding.root);
        }
        out
    }

    /// Returns typed references for every durable object selected by this
    /// checked manifest, in relation, basis, authority order.
    ///
    /// A store can use this list as its closure admission set.  The private
    /// proof marker on each item means the list cannot be forged from roots
    /// copied out of a raw manifest.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when this manifest is still untrusted or
    /// has incomplete relation coverage.
    pub fn closure_refs(&self) -> Result<Vec<ClosureRef>, WorkspaceError> {
        self.validate()?;
        let mut refs = Vec::with_capacity(
            self.relations
                .len()
                .saturating_add(self.basis.len())
                .saturating_add(1),
        );
        refs.extend(
            self.relations.iter().map(|binding| {
                ClosureRef::new(ClosureKind::Relation, binding.schema, binding.root)
            }),
        );
        refs.extend(
            self.basis
                .iter()
                .map(|binding| ClosureRef::new(ClosureKind::Basis, binding.schema, binding.root)),
        );
        let authority = self
            .authority_closure()
            .ok_or(WorkspaceError::UnverifiedAuthority)?;
        refs.push(ClosureRef::new(
            ClosureKind::Authority,
            authority.schema,
            authority.root,
        ));
        Ok(refs)
    }

    /// Returns whether this manifest carries typed closure proofs.
    #[must_use]
    pub const fn is_checked(&self) -> bool {
        self.checked
    }

    /// Returns the authority object closure, when this manifest is checked.
    #[must_use]
    pub const fn authority_closure(&self) -> Option<ObjectClosure> {
        if !self.checked {
            return None;
        }
        match self.authority_schema {
            Some(schema) => Some(ObjectClosure {
                schema,
                root: self.authority,
                _proof: ClosureProof,
            }),
            None => None,
        }
    }

    fn validate_ordering(&self) -> Result<(), WorkspaceError> {
        if self
            .relations
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceError::NonCanonicalRelations);
        }
        if self
            .basis
            .windows(2)
            .any(|window| window[0].schema >= window[1].schema)
        {
            return Err(WorkspaceError::NonCanonicalBasis);
        }
        Ok(())
    }

    fn validate_closure(&self) -> Result<(), WorkspaceError> {
        if !self.checked {
            return Err(WorkspaceError::UnverifiedManifest);
        }
        if self.authority_schema.is_none() {
            return Err(WorkspaceError::UnverifiedAuthority);
        }
        if !self.coverage.is_bound() {
            return Err(WorkspaceError::UnverifiedCoverage);
        }
        if self.relations.iter().any(|binding| !binding.checked) {
            return Err(WorkspaceError::UnverifiedRelation);
        }
        if self.relations.iter().any(|binding| !binding.complete) {
            return Err(WorkspaceError::IncompleteRelation);
        }
        if self.basis.iter().any(|binding| !binding.checked) {
            return Err(WorkspaceError::UnverifiedBasis);
        }
        Ok(())
    }

    /// Returns the manifest schema version.
    #[must_use]
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns selected relation bindings in canonical order.
    #[must_use]
    pub fn relations(&self) -> &[RelationBinding] {
        &self.relations
    }

    /// Returns basis bindings in canonical order.
    #[must_use]
    pub fn basis(&self) -> &[BasisBinding] {
        &self.basis
    }

    /// Returns the authority identity.
    #[must_use]
    pub const fn authority(&self) -> &[u8; ID_BYTES] {
        &self.authority
    }

    /// Returns the coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Revalidates ordering invariants on this manifest.
    ///
    /// # Errors
    ///
    /// Returns the same canonical-order errors as [`Self::new`].
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        if self.schema != u32::from(CANONICAL_VERSION) {
            return Err(WorkspaceError::SchemaMismatch);
        }
        self.validate_ordering()?;
        self.validate_closure()
    }
}

fn append_bindings(out: &mut Vec<u8>, bindings: &[RelationBinding]) {
    append_field(out, |inner| {
        for binding in bindings {
            append_binding(inner, binding.schema, binding.root);
        }
    });
}

fn same_relation_bindings(left: &[RelationBinding], right: &[RelationBinding]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.schema == right.schema && left.root == right.root)
}

fn same_basis_bindings(left: &[BasisBinding], right: &[BasisBinding]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.schema == right.schema && left.root == right.root)
}

fn same_transition_list(left: &[RelationTransition], right: &[RelationTransition]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.schema == right.schema
                && left.base == right.base
                && left.target == right.target
                && left.delta == right.delta
                && left.canonical_changes == right.canonical_changes
        })
}

fn append_binding(out: &mut Vec<u8>, schema: SchemaIdentity, root: [u8; ID_BYTES]) {
    append_field(out, |inner| {
        inner.push(schema.domain());
        inner.extend_from_slice(&schema.ty().to_be_bytes());
        inner.push(schema.version());
        inner.extend_from_slice(&root);
    });
}

fn coverage_tag(coverage: CoverageWitness) -> u8 {
    match coverage.state() {
        Coverage::Complete => 0,
        Coverage::Partial => 1,
        Coverage::Unavailable => 2,
        Coverage::Unsupported => 3,
        Coverage::Closed => 4,
    }
}

fn decode_coverage(reader: &mut WireReader<'_>) -> Result<CoverageWitness, WorkspaceDecodeError> {
    let state = match reader.byte()? {
        0 => Coverage::Complete,
        1 => Coverage::Partial,
        2 => Coverage::Unavailable,
        3 => Coverage::Unsupported,
        4 => Coverage::Closed,
        _ => return Err(WorkspaceDecodeError::InvalidTag),
    };
    let scope = ScopeRoot::from_bytes(reader.digest()?);
    let complete_identity = if state.is_complete() {
        Some(ProducerObservationIdentity::from_canonical_parts(
            reader.digest()?,
            reader.digest()?,
            reader.digest()?,
        ))
    } else {
        None
    };
    Ok(CoverageWitness::from_untrusted_parts(
        state,
        scope,
        complete_identity,
    ))
}
