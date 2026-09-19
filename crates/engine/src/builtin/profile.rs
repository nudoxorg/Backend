//! Compiled profile identities and capability descriptors.

use super::{SEMANTIC_OUTPUT_BODY_BYTES, product_dependency_manifest, product_read_manifest};
use backend_execution::{
    AuthorityVersion, OutputEquivalence, ReadManifestId, RecipeId, RecipeSchema, WorkKeySchema,
};
use backend_replication::{
    CapabilityManifest, ImmutableObjectSchema, RecipeCapability, ResourceEnvelope,
    SchemaDescriptor, TransportLimits, VersionRange, WireIdentity,
};
use backend_semantic::DependencyManifest;
use std::sync::Arc;

/// Stable identity bytes for the checked production profile.
pub const PRODUCT_RECIPE_BYTES: &[u8] = b"backend.worker.builtin.semantic-publication.v1";
/// Authority identity preimage.
pub const PRODUCT_AUTHORITY_BYTES: &[u8] =
    b"backend.worker.builtin.semantic-publication.authority.v1";
/// Output-equivalence identity preimage.
pub const PRODUCT_EQUIVALENCE_BYTES: &[u8] =
    b"backend.worker.builtin.semantic-publication.equivalence.v1";
/// Output identity preimage.
pub const PRODUCT_OUTPUT_BYTES: &[u8] = b"backend.worker.builtin.semantic-publication.output.v1";
/// Semantic coverage witness identity preimage.
pub const PRODUCT_WITNESS_BYTES: &[u8] = b"backend.worker.builtin.semantic-publication.coverage.v1";
/// Maximum authenticated relation nodes charged by one Product execution.
pub const PRODUCT_EXECUTION_NODE_BUDGET: u64 = 1_000_000;
/// Fixed mutable-memory reservation for one streaming Product execution.
pub const PRODUCT_EXECUTION_MEMORY_BYTES: u64 = 128 * 1024;

/// Stable identity bytes for the explicitly named compatibility profile.
pub const ECHO_RECIPE_BYTES: &[u8] = b"backend.worker.builtin.echo.v1";
/// Read-manifest identity preimage.
pub const ECHO_READ_BYTES: &[u8] = b"backend.worker.builtin.echo.reads.v1";
/// Authority identity preimage.
pub const ECHO_AUTHORITY_BYTES: &[u8] = b"backend.worker.builtin.echo.authority.v1";
/// Output-equivalence identity preimage.
pub const ECHO_EQUIVALENCE_BYTES: &[u8] = b"backend.worker.builtin.echo.equivalence.v1";
/// Output identity preimage.
pub const ECHO_OUTPUT_BYTES: &[u8] = b"backend.worker.builtin.echo.output.v1";
/// Semantic coverage witness identity preimage.
pub const ECHO_WITNESS_BYTES: &[u8] = b"backend.worker.builtin.echo.semantic.v1";

/// The compiled profile selected by a process host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    /// The versioned relation product used by the production composition.
    Product,
    /// Compatibility material for legacy clients and focused tests.
    EchoFixture,
}

/// The identities and result contract shared by both process adapters.
#[derive(Clone, Copy, Debug)]
pub struct ProfileIds {
    /// Typed recipe identity.
    pub recipe: RecipeId,
    /// Typed semantic read-manifest identity.
    pub read_manifest: ReadManifestId,
    /// Typed authority version identity.
    pub authority: AuthorityVersion,
    /// Typed output-equivalence identity.
    pub equivalence: OutputEquivalence,
    /// Stable result-byte prefix.
    pub output_prefix: &'static [u8],
    /// Coverage witness identity preimage.
    pub witness: &'static [u8],
    /// Whether the profile retains relation input basis bytes in output.
    pub include_basis: bool,
}

/// Immutable profile descriptor built once during process startup.
#[derive(Debug)]
pub struct ProfileDescriptor {
    /// Selected compiled profile kind.
    pub kind: Profile,
    /// Typed identities and result contract.
    pub ids: ProfileIds,
    /// Recipe identity in its wire schema context.
    pub recipe_claim: WireIdentity,
    /// Exact reusable dependency manifest, when the profile supports reuse.
    pub product_manifest: Option<DependencyManifest>,
}

/// Returns the exact fixed result size for one compiled profile.
#[must_use]
pub fn profile_output_len(profile: ProfileIds) -> usize {
    profile
        .output_prefix
        .len()
        .saturating_add(if profile.include_basis {
            SEMANTIC_OUTPUT_BODY_BYTES
        } else {
            0
        })
}

/// Computes the shared identities for one compiled profile.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn profile_ids(profile: &Profile) -> Result<ProfileIds, String> {
    let (recipe, authority_bytes, equivalence, output_prefix, witness) = match profile {
        Profile::Product => (
            PRODUCT_RECIPE_BYTES,
            PRODUCT_AUTHORITY_BYTES,
            PRODUCT_EQUIVALENCE_BYTES,
            PRODUCT_OUTPUT_BYTES,
            PRODUCT_WITNESS_BYTES,
        ),
        Profile::EchoFixture => (
            ECHO_RECIPE_BYTES,
            ECHO_AUTHORITY_BYTES,
            ECHO_EQUIVALENCE_BYTES,
            ECHO_OUTPUT_BYTES,
            ECHO_WITNESS_BYTES,
        ),
    };
    let authority = AuthorityVersion::from_value(authority_bytes);
    let read_bytes = match profile {
        Profile::Product => product_read_manifest(authority)?,
        Profile::EchoFixture => ECHO_READ_BYTES.to_vec(),
    };
    Ok(ProfileIds {
        recipe: RecipeId::from_value(recipe),
        read_manifest: ReadManifestId::from_value(&read_bytes),
        authority,
        equivalence: OutputEquivalence::from_value(equivalence),
        output_prefix,
        witness,
        include_basis: matches!(profile, Profile::Product),
    })
}

/// Builds an immutable descriptor shared by locald validation and worker
/// admission.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn profile_descriptor(profile: Profile) -> Result<Arc<ProfileDescriptor>, String> {
    let ids = profile_ids(&profile)?;
    let recipe_claim = WireIdentity::from_typed(&ids.recipe);
    let product_manifest = matches!(profile, Profile::Product)
        .then(|| product_dependency_manifest(ids))
        .transpose()?;
    Ok(Arc::new(ProfileDescriptor {
        kind: profile,
        ids,
        recipe_claim,
        product_manifest,
    }))
}

/// Builds the bounded result resource envelope shared by owner and worker.
#[must_use]
pub fn execution_resources(profile: ProfileIds) -> ResourceEnvelope {
    let output_len = profile_output_len(profile);
    let input_len = if profile.include_basis { 32_usize } else { 0 };
    ResourceEnvelope {
        // Product execution charges one unit per authenticated tree node. The
        // generous fixed ceiling remains deterministic on the wire while the
        // worker's actual work stays proportional to the version frontier.
        cpu_millis: if profile.include_basis {
            PRODUCT_EXECUTION_NODE_BUDGET.saturating_add(1)
        } else {
            100
        },
        // One canonical node proof is at most 64 KiB. A second node-sized
        // allowance covers decoded leaf values and the depth-first frontier.
        memory_bytes: if profile.include_basis {
            PRODUCT_EXECUTION_MEMORY_BYTES.saturating_add(input_len as u64)
        } else {
            1024
        },
        // The worker charges admitted input bytes before execution and result
        // bytes after it. Keep that lifecycle accounting in this shared
        // profile constructor so owner and worker cannot budget different
        // phases of the same recipe.
        network_bytes: input_len.saturating_add(output_len) as u64,
        storage_bytes: output_len as u64,
        output_bytes: output_len as u64,
        processes: 1,
        wall_millis: if profile.include_basis { 10_000 } else { 1_000 },
    }
}

/// Builds the capabilities advertised by the compiled profile.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn execution_manifest(
    limits: TransportLimits,
    profile: &ProfileDescriptor,
) -> Result<CapabilityManifest, String> {
    let ids = profile.ids;
    let mut schemas = vec![
        SchemaDescriptor::of::<RecipeSchema>(),
        SchemaDescriptor::of::<backend_execution::ReadManifestSchema>(),
        SchemaDescriptor::of::<backend_execution::AuthorityVersionSchema>(),
        SchemaDescriptor::of::<WorkKeySchema>(),
        SchemaDescriptor::of::<ImmutableObjectSchema>(),
    ];
    schemas.sort_by_key(|schema| (schema.domain, schema.type_id));
    let max_frame =
        u32::try_from(limits.max_frame).map_err(|_| "frame bound overflow".to_owned())?;
    let max_output = profile_output_len(ids);
    if limits.max_object < max_output as u64 {
        return Err("object bound cannot retain the recipe output".to_owned());
    }
    // Immutable source rows and relation nodes are input-plane objects. Their
    // admitted size is governed by the transport object bound, independently
    // of the much smaller deterministic result envelope below.
    let max_object = limits.max_object;
    let max_chunk = u32::try_from(limits.max_chunk.min(limits.max_frame))
        .map_err(|_| "chunk bound overflow".to_owned())?
        .min(max_frame.max(1));
    Ok(CapabilityManifest {
        protocol: VersionRange { min: 1, max: 1 },
        schemas,
        recipes: vec![RecipeCapability {
            recipe: WireIdentity::from_typed(&ids.recipe),
            versions: VersionRange { min: 1, max: 1 },
        }],
        max_object,
        max_chunk,
        max_frame: max_frame.max(1),
        max_ranges: 4,
        max_resources: execution_resources(ids),
    })
}
