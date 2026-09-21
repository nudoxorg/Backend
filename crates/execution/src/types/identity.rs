//! Typed identities used by the execution control plane.
//!
//! Execution deliberately does not have a second hash type. Every identity
//! that crosses this crate is an [`ObjectVersion`] from `backend-version` and
//! therefore carries a schema marker in its Rust type as well as in its
//! canonical hash domain.

use backend_version::{ObjectVersion, Relation, Schema, StateRoot};
use std::fmt;

/// Canonical schema for an operator/ABI recipe identity.
#[derive(Debug)]
pub struct RecipeSchema;
impl Schema for RecipeSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0001;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for a complete read/dependency manifest.
#[derive(Debug)]
pub struct ReadManifestSchema;
impl Schema for ReadManifestSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0002;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for the workspace authority/capability revision.
#[derive(Debug)]
pub struct AuthorityVersionSchema;
impl Schema for AuthorityVersionSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0003;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for the declared output equivalence contract.
#[derive(Debug)]
pub struct OutputEquivalenceSchema;
impl Schema for OutputEquivalenceSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0004;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for an immutable materialized result version.
#[derive(Debug)]
pub struct OutputSchema;
impl Schema for OutputSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0005;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema used to hash a complete versioned work identity.
#[derive(Debug)]
pub struct WorkKeySchema;

/// The framed fields that make up a [`WorkKey`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkKeyMaterial {
    /// Relation schema domain for the input root.
    relation_domain: u8,
    /// Relation schema type for the input root.
    relation_type: u16,
    /// Relation schema version for the input root.
    relation_version: u8,
    /// Recipe schema version.
    recipe: [u8; 32],
    /// Input relation root.
    input: [u8; 32],
    /// Read manifest schema version.
    read_manifest: [u8; 32],
    /// Workspace authority schema version.
    authority: [u8; 32],
    /// Output equivalence schema version.
    output_equivalence: [u8; 32],
}

impl Schema for WorkKeySchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 0x0006;
    type Value = WorkKeyMaterial;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.push(value.relation_domain);
        out.extend_from_slice(&value.relation_type.to_be_bytes());
        out.push(value.relation_version);
        // `ObjectVersion` already length-frames the complete schema value. A
        // field frame here prevents concatenation ambiguity inside that value.
        for field in [
            &value.recipe,
            &value.input,
            &value.read_manifest,
            &value.authority,
            &value.output_equivalence,
        ] {
            out.extend_from_slice(&(field.len() as u64).to_be_bytes());
            out.extend_from_slice(field);
        }
    }
}

/// Recipe identity, including its operator ABI and declared configuration.
pub type RecipeId = ObjectVersion<RecipeSchema>;
/// Exact positive/negative/range read manifest identity.
pub type ReadManifestId = ObjectVersion<ReadManifestSchema>;
/// Version of the workspace authority/capability used to produce a result.
pub type AuthorityVersion = ObjectVersion<AuthorityVersionSchema>;
/// Contract defining which outputs are equivalent for reuse or hedging.
pub type OutputEquivalence = ObjectVersion<OutputEquivalenceSchema>;
/// Version of one accepted materialized output.
pub type OutputVersion = ObjectVersion<OutputSchema>;

/// A complete versioned identity for pure work.
///
/// The marker schemas are intentionally independent. A recipe, read
/// manifest, authority, and output contract cannot accidentally be supplied
/// in a single unconstrained schema parameter.
pub struct VersionedWorkIdentity<R: Relation> {
    /// Operator ABI/configuration identity.
    pub recipe: RecipeId,
    /// Exact relation state root read by the work.
    pub input: StateRoot<R>,
    /// Complete positive, negative, range, and environment read identity.
    pub read_manifest: ReadManifestId,
    /// Workspace authority/capability revision.
    pub authority: AuthorityVersion,
    /// Declared equivalence contract for outputs.
    pub output_equivalence: OutputEquivalence,
}

impl<R: Relation> Copy for VersionedWorkIdentity<R> {}

impl<R: Relation> Clone for VersionedWorkIdentity<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> PartialEq for VersionedWorkIdentity<R> {
    fn eq(&self, other: &Self) -> bool {
        self.recipe == other.recipe
            && self.input == other.input
            && self.read_manifest == other.read_manifest
            && self.authority == other.authority
            && self.output_equivalence == other.output_equivalence
    }
}

impl<R: Relation> Eq for VersionedWorkIdentity<R> {}

impl<R: Relation> fmt::Debug for VersionedWorkIdentity<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VersionedWorkIdentity")
            .field("recipe", &self.recipe)
            .field("input", &self.input)
            .field("read_manifest", &self.read_manifest)
            .field("authority", &self.authority)
            .field("output_equivalence", &self.output_equivalence)
            .finish()
    }
}

impl<R: Relation> VersionedWorkIdentity<R> {
    /// Creates a complete work identity from its independently typed fields.
    #[must_use]
    pub const fn new(
        recipe: RecipeId,
        input: StateRoot<R>,
        read_manifest: ReadManifestId,
        authority: AuthorityVersion,
        output_equivalence: OutputEquivalence,
    ) -> Self {
        Self {
            recipe,
            input,
            read_manifest,
            authority,
            output_equivalence,
        }
    }

    /// Derives the stable work key for this identity.
    #[must_use]
    pub fn work_key(&self) -> WorkKey {
        derive_work_key(*self)
    }

    /// Admits the complete identity once and retains its derived key for all
    /// downstream boundaries. Consumers should pass this ticket instead of
    /// repeatedly deriving the same content address.
    #[must_use]
    pub fn admit(&self) -> AdmittedWork<R> {
        AdmittedWork::new(*self)
    }

    /// Admits this immutable identity as a [`WorkTicket`].
    #[must_use]
    pub fn ticket(&self) -> WorkTicket<R> {
        WorkTicket::new(*self)
    }
}

/// Immutable work admission ticket carrying identity and its exact key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmittedWork<R: Relation> {
    identity: VersionedWorkIdentity<R>,
    key: WorkKey,
}

/// Immutable, engine admitted work ticket.
///
/// A ticket carries the complete typed identity and the derived key together;
/// downstream control paths can therefore pass one capability instead of
/// repeatedly deriving or reconstructing identity fields.
pub type WorkTicket<R> = AdmittedWork<R>;

impl<R: Relation> AdmittedWork<R> {
    /// Creates one ticket and derives its key exactly once.
    #[must_use]
    pub fn new(identity: VersionedWorkIdentity<R>) -> Self {
        Self {
            key: derive_work_key(identity),
            identity,
        }
    }

    /// Returns the original typed identity.
    #[must_use]
    pub const fn identity(&self) -> VersionedWorkIdentity<R> {
        self.identity
    }

    /// Returns the stable key derived during admission.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }
}

/// Stable semantic identity for one in-flight or reusable pure computation.
///
/// The payload is an `ObjectVersion<WorkKeySchema>`, rather than a local
/// untyped hash. The alias leaves the schema marker in the public type so a
/// work key cannot be confused with any other object version.
pub type WorkKey = ObjectVersion<WorkKeySchema>;

/// Derives the process-local acquisition work key used by the registry and
/// source acquisition coordinator.
///
/// Acquisition is a different protocol from semantic execution, but it still
/// uses the same bounded [`WorkInterner`] and therefore must use the same
/// content-addressed key type.  The five fields are length-framed before they
/// are folded into the existing work-key schema; callers cannot make a key by
/// concatenating ambiguous strings.
#[must_use]
pub fn acquisition_work_key(
    source: [u8; 32],
    coordinate: &[u8],
    artifact: [u8; 32],
    schema: u16,
    policy_epoch: u64,
) -> WorkKey {
    let mut coordinate_digest = blake3::Hasher::new();
    coordinate_digest.update(b"backend.acquisition.coordinate.v1\0");
    coordinate_digest.update(&(coordinate.len() as u64).to_be_bytes());
    coordinate_digest.update(coordinate);
    let coordinate = *coordinate_digest.finalize().as_bytes();
    let mut policy = [0_u8; 32];
    policy[..8].copy_from_slice(&policy_epoch.to_be_bytes());
    let material = WorkKeyMaterial {
        relation_domain: 0x72,
        relation_type: schema,
        relation_version: 1,
        recipe: source,
        input: artifact,
        read_manifest: coordinate,
        authority: policy,
        output_equivalence: [0; 32],
    };
    ObjectVersion::<WorkKeySchema>::from_value(&material)
}

fn derive_work_key<R: Relation>(identity: VersionedWorkIdentity<R>) -> WorkKey {
    let material = WorkKeyMaterial {
        relation_domain: R::DOMAIN,
        relation_type: R::TYPE,
        relation_version: R::VERSION,
        recipe: identity.recipe.to_bytes(),
        input: identity.input.to_bytes(),
        read_manifest: identity.read_manifest.to_bytes(),
        authority: identity.authority.to_bytes(),
        output_equivalence: identity.output_equivalence.to_bytes(),
    };
    ObjectVersion::<WorkKeySchema>::from_value(&material)
}

/// Internal erased identity binding retained by the attempt registry.
///
/// The relation marker cannot be erased from a `StateRoot<R>` at runtime, so
/// the schema context is retained alongside its bytes. This is only a
/// comparison token; it is never exposed as a substitute for `StateRoot<R>`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IdentityBinding {
    pub key: WorkKey,
    pub relation_domain: u8,
    pub relation_type: u16,
    pub relation_version: u8,
    pub input: [u8; 32],
    pub recipe: [u8; 32],
    pub read_manifest: [u8; 32],
    pub authority: [u8; 32],
    pub output_equivalence: [u8; 32],
}

impl<R: Relation> From<VersionedWorkIdentity<R>> for IdentityBinding {
    fn from(identity: VersionedWorkIdentity<R>) -> Self {
        let ticket = AdmittedWork::new(identity);
        Self::from_ticket(&ticket)
    }
}

impl IdentityBinding {
    pub(crate) fn from_ticket<R: Relation>(ticket: &AdmittedWork<R>) -> Self {
        let identity = ticket.identity();
        Self {
            key: ticket.key(),
            relation_domain: R::DOMAIN,
            relation_type: R::TYPE,
            relation_version: R::VERSION,
            input: identity.input.to_bytes(),
            recipe: identity.recipe.to_bytes(),
            read_manifest: identity.read_manifest.to_bytes(),
            authority: identity.authority.to_bytes(),
            output_equivalence: identity.output_equivalence.to_bytes(),
        }
    }
}

/// Result type used by the interner when a result is terminally published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionReceipt {
    /// Work identity whose live entry was completed.
    pub(super) key: WorkKey,
    /// Accepted immutable output version.
    pub(super) output: OutputVersion,
}

impl CompletionReceipt {
    /// Returns the completed work key.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the immutable output published to followers.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }
}
