//! Capability, schema, and opaque recipe negotiation.

use crate::{
    ReplicationError, ResourceEnvelope, SchemaDescriptor, TransportLimits, VersionRange,
    WireIdentity,
};

/// An opaque recipe capability advertised by a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecipeCapability {
    /// Untrusted recipe identity and schema context. The execution owner
    /// admits it against its own typed recipe ID.
    pub recipe: WireIdentity,
    /// Recipe ABI/schema version range.
    pub versions: VersionRange,
}

/// Bounded capabilities and schemas advertised by a local or remote peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityManifest {
    /// Supported transport protocol versions.
    pub protocol: VersionRange,
    /// Supported schema descriptors.
    pub schemas: Vec<SchemaDescriptor>,
    /// Supported pure recipes.
    pub recipes: Vec<RecipeCapability>,
    /// Maximum immutable object size.
    pub max_object: u64,
    /// Maximum chunk payload.
    pub max_chunk: u32,
    /// Maximum encoded frame.
    pub max_frame: u32,
    /// Maximum sparse ranges per request.
    pub max_ranges: u32,
    /// Maximum pure compute resources accepted by this peer.
    pub max_resources: ResourceEnvelope,
}
impl CapabilityManifest {
    /// Validates ordering, duplicates, and advertised allocation limits.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidCapabilities`] when the advertised
    /// lists or limits are malformed.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.schemas.len() > limits.max_capabilities
            || self.recipes.len() > limits.max_capabilities
            || self.max_object == 0
            || self.max_chunk == 0
            || self.max_frame == 0
            || self.max_ranges == 0
            || self.max_chunk > self.max_frame
            || self.protocol.min > self.protocol.max
            || self
                .schemas
                .iter()
                .any(|schema| schema.versions.min > schema.versions.max)
            || self
                .recipes
                .iter()
                .any(|recipe| recipe.versions.min > recipe.versions.max)
            || self
                .schemas
                .windows(2)
                .any(|pair| (pair[0].domain, pair[0].type_id) >= (pair[1].domain, pair[1].type_id))
            || self
                .recipes
                .windows(2)
                .any(|pair| pair[0].recipe >= pair[1].recipe)
        {
            return Err(ReplicationError::InvalidCapabilities);
        }
        Ok(())
    }
    /// Negotiates protocol, schema, recipe, and shared resource limits.
    ///
    /// # Errors
    ///
    /// Returns a negotiation error when protocol/schema versions do not
    /// overlap or either advertisement is malformed.
    pub fn negotiate(
        &self,
        peer: &Self,
        limits: TransportLimits,
    ) -> Result<NegotiatedCapabilities, ReplicationError> {
        self.validate(limits)?;
        peer.validate(limits)?;
        let protocol = self.protocol.negotiate(peer.protocol)?;
        let mut schemas = Vec::new();
        for local in &self.schemas {
            if let Some(remote) = peer.schemas.iter().find(|candidate| {
                candidate.domain == local.domain && candidate.type_id == local.type_id
            }) {
                schemas.push(local.negotiate(*remote)?);
            }
        }
        if schemas.is_empty() {
            return Err(ReplicationError::NoCommonSchema);
        }
        let mut recipes = Vec::new();
        for local in &self.recipes {
            if let Some(remote) = peer
                .recipes
                .iter()
                .find(|candidate| candidate.recipe == local.recipe)
            {
                let version = local.versions.negotiate(remote.versions)?;
                recipes.push((local.recipe, version));
            }
        }
        if (!self.recipes.is_empty() || !peer.recipes.is_empty()) && recipes.is_empty() {
            return Err(ReplicationError::NoCommonRecipe);
        }
        let negotiated_limits = TransportLimits {
            max_frame: limits
                .max_frame
                .min(self.max_frame as usize)
                .min(peer.max_frame as usize),
            max_chunk: limits
                .max_chunk
                .min(self.max_chunk as usize)
                .min(peer.max_chunk as usize),
            max_object: limits.max_object.min(self.max_object).min(peer.max_object),
            max_objects: limits.max_objects,
            max_ranges: limits
                .max_ranges
                .min(self.max_ranges as usize)
                .min(peer.max_ranges as usize),
            max_capabilities: limits.max_capabilities,
            max_key_bytes: limits.max_key_bytes,
            max_inputs: limits.max_inputs,
        };
        Ok(NegotiatedCapabilities {
            protocol,
            schemas,
            recipes,
            limits: negotiated_limits,
            max_resources: self
                .max_resources
                .intersect(peer.max_resources)
                .clamp_transport(negotiated_limits),
        })
    }
}

/// Negotiated protocol/schema/recipe capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedCapabilities {
    /// Selected transport protocol version.
    pub protocol: u16,
    /// Selected schema versions.
    pub schemas: Vec<SchemaDescriptor>,
    /// Recipe identities and selected recipe ABI versions.
    pub recipes: Vec<(WireIdentity, u16)>,
    /// Shared bounded transfer limits.
    pub limits: TransportLimits,
    /// Element-wise common pure compute ceiling.
    pub max_resources: ResourceEnvelope,
}
