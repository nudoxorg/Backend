//! Caller-owned live-byte residency for exact typed V2 history generations.
//!
//! Generation proofs remain exact-generation objects. Physical object bytes
//! are stored once by `(ObjectId, schema, length)` and may be supplied to a
//! neighboring generation only as inputs to its full V2 verifier.

use super::*;
use std::mem::size_of;

const SKETCH_ROWS: usize = 4;
const SKETCH_WIDTH: usize = 1024;
const SKETCH_AGE_AFTER: u64 = 4 * 1024 * 4;
const PROBATION_PERCENT: u64 = 25;
const MAX_RESIDENT_ENTRIES: usize = 1024;

/// Bounded caller-owned cache. Mutating operations require `&mut self`, so a
/// replay borrow prevents eviction for exactly as long as its object views and
/// fresh FileStore GC pin are live.
pub struct TypedV2HistoryResidencyCache {
    pub(super) byte_budget: u64,
    max_entries: usize,
    pub(super) entries: Vec<TypedV2ResidentEntry>,
    ghosts: Vec<TypedV2ResidentGhost>,
    pub(super) objects: Vec<TypedV2ResidentObject>,
    sketch: [[u8; SKETCH_WIDTH]; SKETCH_ROWS],
    observations: u64,
    sketch_ages: u64,
    hits: u64,
    cold_misses: u64,
    pub(super) payload_bytes_read: u64,
    resident_payload_bytes_reused: u64,
    payload_bytes_served: u64,
    admissions: u64,
    admission_rejections: u64,
    evictions: u64,
    shared_object_reuses: u64,
    transient_high_water_bytes: u64,
}

/// Deterministic logical-byte and replay-work counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TypedV2HistoryResidencyMetrics {
    /// Entire retained cache footprint, including reserved table slots.
    pub accounted_bytes: u64,
    /// Configured retained-byte ceiling.
    pub byte_budget: u64,
    /// Bytes attributed to probation, including uniquely owned payloads.
    pub probation_bytes: u64,
    /// Bytes attributed to protected entries and their shared payloads.
    pub protected_bytes: u64,
    /// Number of exact verified generations retained.
    pub entries: usize,
    /// Number of shared content-addressed physical objects retained once.
    pub resident_objects: usize,
    /// Unique payload bytes in the object pool.
    pub resident_object_bytes: u64,
    /// Sum of payload bytes if every generation had a private full copy.
    pub generation_wide_payload_bytes: u64,
    /// Generation-wide duplicate bytes avoided by the object pool.
    pub deduplicated_payload_bytes: u64,
    /// Number of exact cold-miss keys retained in the admission ghost table.
    pub ghost_entries: usize,
    /// Target-key bytes held by the admission ghost table.
    pub ghost_bytes: u64,
    /// Requests served from exact-generation owned proof and bytes.
    pub hits: u64,
    /// Requests requiring a complete V2 verifier run.
    pub cold_misses: u64,
    /// Physical FileStore payload bytes streamed into verification spools.
    pub payload_bytes_read: u64,
    /// Payload bytes supplied from an already verified shared object pool.
    pub resident_payload_bytes_reused: u64,
    /// Exact resident payload bytes exposed through replay handles.
    pub payload_bytes_served: u64,
    /// Fully verified exact generations admitted to the cache.
    pub admissions: u64,
    /// Valid generations left cold by byte or entry limits.
    pub admission_rejections: u64,
    /// Exact generation entries discarded by replacement.
    pub evictions: u64,
    /// Objects deduplicated when admitting a neighboring generation.
    pub shared_object_reuses: u64,
    /// Estimated live-residency peak: retained bytes plus locator/index
    /// metadata and one fixed I/O buffer while cold spool construction runs.
    /// Payload spool bytes are disk-backed; verifier-internal allocations and
    /// allocator bookkeeping are outside this estimate.
    pub transient_high_water_bytes: u64,
    /// Number of fixed-sketch halving passes.
    pub sketch_ages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResidentTier {
    Probation,
    Protected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TypedV2ResidentKey {
    pub(super) target: crate::SemanticTargetKey,
    pub(super) commit: crate::HistoryCommitId,
    pub(super) roots: crate::HistoryTypedV2RootClaim,
    pub(super) tier: SemanticTypedPlaneVerificationTierV2,
    pub(super) jumbo_limits: JumboRopeLimits,
}

pub(super) struct TypedV2ResidentEntry {
    pub(super) key: TypedV2ResidentKey,
    digest: [u8; 32],
    pub(super) proof: VerifiedTypedPlaneContentV2,
    /// Canonical manifest bytes, already checked against this exact locator
    /// identity on a cold admission. Keeping bytes avoids decoding every
    /// descriptor on a warm hit.
    pub(super) manifest_bytes: Box<[u8]>,
    /// Locator-bound lineage bytes are retained with the same exact proof.
    pub(super) lineage_edge_set: Option<Box<[u8]>>,
    /// Per-generation object mapping and stable closure offsets. The offsets
    /// are metadata only; payload storage is shared by the object pool.
    pub(super) members: Box<[TypedV2SpoolMember]>,
    pub(super) segments: Box<[HistoryTypedV2SegmentObject]>,
    pub(super) jumbo: Box<[HistoryTypedV2JumboObject]>,
    metadata_bytes: u64,
    recent_hits: u8,
    last_access: u64,
    tier: ResidentTier,
}

struct TypedV2ResidentGhost {
    key: TypedV2ResidentKey,
    cold_misses: u8,
    last_access: u64,
}

#[derive(Debug)]
pub(super) struct TypedV2ResidentObject {
    pub(super) id: ObjectId,
    pub(super) schema: SchemaIdentity,
    pub(super) byte_length: u64,
    pub(super) payload: Box<[u8]>,
    owners: usize,
    protected_owners: usize,
}

/// A proof-bearing, borrow-scoped generation view. Object payload slices are
/// borrowed from the cache's unique-object pool; retaining this handle holds
/// both the exclusive cache borrow and a fresh FileStore GC pin.
#[derive(Debug)]
pub struct TypedV2HistoryResidentReplay<'cache> {
    pub(super) commit: crate::AdmittedHistoryCommit,
    pub(super) manifest_bytes: &'cache [u8],
    pub(super) proof: &'cache VerifiedTypedPlaneContentV2,
    pub(super) members: &'cache [TypedV2SpoolMember],
    pub(super) segments: &'cache [HistoryTypedV2SegmentObject],
    pub(super) jumbo: &'cache [HistoryTypedV2JumboObject],
    pub(super) objects: &'cache [TypedV2ResidentObject],
    pub(super) lineage_edge_set: Option<&'cache [u8]>,
    pub(super) _gc_pin: GcPinGuard,
}

impl TypedV2HistoryResidentReplay<'_> {
    /// Current admitted commit freshly loaded for this ancestry-checked read.
    #[must_use]
    pub const fn commit(&self) -> &crate::AdmittedHistoryCommit {
        &self.commit
    }

    /// Exact canonical manifest bytes retained with the generation proof.
    /// The bytes are not decoded again on a warm hit.
    #[must_use]
    pub const fn manifest_bytes(&self) -> &[u8] {
        self.manifest_bytes
    }

    /// Exact-generation semantic proof. This proof is never shared across
    /// distinct commits, even when their content root or object pool matches.
    #[must_use]
    pub const fn content(&self) -> &VerifiedTypedPlaneContentV2 {
        self.proof
    }

    /// Semantic segment to FileStore object mappings copied from the exact
    /// validated locator. These mappings stay available without re-decoding
    /// the locator on an exact warm hit.
    #[must_use]
    pub const fn segment_objects(&self) -> &[HistoryTypedV2SegmentObject] {
        self.segments
    }

    /// Jumbo rope to FileStore object mappings copied from the exact validated
    /// locator and retained with the exact generation proof.
    #[must_use]
    pub const fn jumbo_objects(&self) -> &[HistoryTypedV2JumboObject] {
        self.jumbo
    }

    /// Returns locator-bound lineage candidate bytes. They remain explicitly
    /// unproven; callers must use `lineage_candidates` for endpoint checks.
    #[must_use]
    pub const fn lineage_edge_set_bytes(&self) -> Option<&[u8]> {
        self.lineage_edge_set
    }

    /// Parses the retained locator bytes and rechecks commit/generation
    /// endpoints. This remains an unproven lineage view, matching cold replay.
    pub fn lineage_candidates(
        &self,
    ) -> Result<Option<crate::UnprovenTypedLineageEdgeSetV1<'_>>, crate::LineageEdgeSetErrorV1>
    {
        let Some(bytes) = self.lineage_edge_set else {
            return Ok(None);
        };
        let view = crate::BorrowedTypedLineageEdgeSetV1::parse(bytes)?;
        if self.commit.parents().first().copied() != Some(view.parent_commit()) {
            return Err(crate::LineageEdgeSetErrorV1::EndpointMismatch);
        }
        if view.child_generation_claim() != self.proof.generation_root().as_bytes() {
            return Err(crate::LineageEdgeSetErrorV1::GenerationRootMismatch);
        }
        Ok(Some(crate::UnprovenTypedLineageEdgeSetV1::root_bound(
            view,
            self.commit.identity(),
        )))
    }

    /// Returns a member's verified schema and shared owned payload.
    #[must_use]
    pub fn object_payload(&self, object: UntrustedObjectId) -> Option<(SchemaIdentity, &[u8])> {
        self.object_payload_with_offset(object)
            .map(|(schema, _offset, payload)| (schema, payload))
    }

    /// Returns a member's schema, canonical closure offset, and shared owned
    /// payload. Offsets are from this exact generation's physical-ID ordered
    /// closure layout and remain stable even though object bytes are pooled.
    #[must_use]
    pub fn object_payload_with_offset(
        &self,
        object: UntrustedObjectId,
    ) -> Option<(SchemaIdentity, u64, &[u8])> {
        let member = self
            .members
            .binary_search_by(|member| member.id.as_bytes().cmp(object.as_bytes()))
            .ok()
            .and_then(|index| self.members.get(index))?;
        let pool_index = self
            .objects
            .binary_search_by(|candidate| candidate.id.as_bytes().cmp(member.id.as_bytes()))
            .ok()?;
        let pooled = self.objects.get(pool_index)?;
        if pooled.schema != member.schema || pooled.byte_length != member.byte_length {
            return None;
        }
        Some((member.schema, member.offset, &pooled.payload))
    }

    /// Iterates exact object identities, schemas, closure offsets, and payload
    /// borrows in ascending physical object-ID order.
    pub fn object_payloads(
        &self,
    ) -> impl Iterator<Item = (ObjectId, SchemaIdentity, u64, &[u8])> + '_ {
        self.members.iter().filter_map(|member| {
            let index = self
                .objects
                .binary_search_by(|candidate| candidate.id.as_bytes().cmp(member.id.as_bytes()))
                .ok()?;
            let object = self.objects.get(index)?;
            (object.schema == member.schema && object.byte_length == member.byte_length)
                .then_some((member.id, member.schema, member.offset, &object.payload))
        })
    }

    /// Sum of member payload bytes in this exact generation.
    #[must_use]
    pub fn payload_byte_len(&self) -> u64 {
        self.members.iter().fold(0_u64, |total, member| {
            total.saturating_add(member.byte_length)
        })
    }
}

/// Result of replay through bounded live residency. Refused/oversize cache
/// admission returns an ordinary cold replay token, never a proof-only entry.
#[derive(Debug)]
pub enum TypedV2HistoryResidencyReplay<'cache> {
    Resident(TypedV2HistoryResidentReplay<'cache>),
    Cold(crate::TypedV2HistoryReplay),
}

pub(super) enum TypedV2Admission {
    Resident(usize),
    Rejected(VerifiedTypedPlaneContentV2),
}

impl TypedV2HistoryResidencyCache {
    /// Creates an empty cache with a strict retained-byte and entry ceiling.
    pub fn new(byte_budget: u64, max_entries: usize) -> Result<Self, String> {
        if max_entries == 0 || max_entries > MAX_RESIDENT_ENTRIES {
            return Err("typed V2 residency entry limit is outside its fixed bounds".to_owned());
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(max_entries)
            .map_err(|_| "typed V2 residency table allocation failed".to_owned())?;
        let mut ghosts = Vec::new();
        ghosts
            .try_reserve_exact(max_entries)
            .map_err(|_| "typed V2 residency admission-ghost allocation failed".to_owned())?;
        let cache = Self {
            byte_budget,
            max_entries,
            entries,
            ghosts,
            objects: Vec::new(),
            sketch: [[0; SKETCH_WIDTH]; SKETCH_ROWS],
            observations: 0,
            sketch_ages: 0,
            hits: 0,
            cold_misses: 0,
            payload_bytes_read: 0,
            resident_payload_bytes_reused: 0,
            payload_bytes_served: 0,
            admissions: 0,
            admission_rejections: 0,
            evictions: 0,
            shared_object_reuses: 0,
            transient_high_water_bytes: 0,
        };
        if cache.fixed_bytes() > byte_budget {
            return Err(
                "typed V2 residency budget is smaller than its fixed table and sketch".to_owned(),
            );
        }
        Ok(cache)
    }

    /// Creates a standard 64 MiB cache with room for 64 exact generations.
    pub fn standard() -> Result<Self, String> {
        Self::new(64 * 1024 * 1024, 64)
    }

    /// Reports retained bytes, generation-wide duplication avoided, and
    /// measured FileStore versus pool-supplied payload work.
    #[must_use]
    pub fn metrics(&self) -> TypedV2HistoryResidencyMetrics {
        let probation_bytes = self.tier_bytes(ResidentTier::Probation);
        let protected_bytes = self.tier_bytes(ResidentTier::Protected);
        let resident_object_bytes = self
            .objects
            .iter()
            .fold(0_u64, |sum, object| sum.saturating_add(object.byte_length));
        let generation_wide_payload_bytes = self.entries.iter().fold(0_u64, |sum, entry| {
            sum.saturating_add(entry.members.iter().fold(0_u64, |member_sum, member| {
                member_sum.saturating_add(member.byte_length)
            }))
        });
        TypedV2HistoryResidencyMetrics {
            accounted_bytes: self.accounted_bytes(),
            byte_budget: self.byte_budget,
            probation_bytes,
            protected_bytes,
            entries: self.entries.len(),
            resident_objects: self.objects.len(),
            resident_object_bytes,
            generation_wide_payload_bytes,
            deduplicated_payload_bytes: generation_wide_payload_bytes
                .saturating_sub(resident_object_bytes),
            ghost_entries: self.ghosts.len(),
            ghost_bytes: self.ghost_bytes(),
            hits: self.hits,
            cold_misses: self.cold_misses,
            payload_bytes_read: self.payload_bytes_read,
            resident_payload_bytes_reused: self.resident_payload_bytes_reused,
            payload_bytes_served: self.payload_bytes_served,
            admissions: self.admissions,
            admission_rejections: self.admission_rejections,
            evictions: self.evictions,
            shared_object_reuses: self.shared_object_reuses,
            transient_high_water_bytes: self.transient_high_water_bytes,
            sketch_ages: self.sketch_ages,
        }
    }

    fn fixed_bytes(&self) -> u64 {
        u64::try_from(size_of::<Self>())
            .unwrap_or(u64::MAX)
            .saturating_add(capacity_bytes::<TypedV2ResidentEntry>(
                self.entries.capacity(),
            ))
            .saturating_add(capacity_bytes::<TypedV2ResidentGhost>(
                self.ghosts.capacity(),
            ))
            .saturating_add(capacity_bytes::<TypedV2ResidentObject>(
                self.objects.capacity(),
            ))
    }

    fn accounted_bytes(&self) -> u64 {
        let entry_bytes = self.entries.iter().fold(0_u64, |total, entry| {
            total.saturating_add(entry.metadata_bytes)
        });
        let object_payload_bytes = self.objects.iter().fold(0_u64, |total, object| {
            total.saturating_add(object.byte_length)
        });
        self.fixed_bytes()
            .saturating_add(entry_bytes)
            .saturating_add(object_payload_bytes)
            .saturating_add(self.ghost_bytes())
    }

    fn ghost_bytes(&self) -> u64 {
        self.ghosts.iter().fold(0_u64, |total, ghost| {
            total
                .saturating_add(u64::try_from(ghost.key.target.package().len()).unwrap_or(u64::MAX))
                .saturating_add(
                    u64::try_from(ghost.key.target.coordinate().len()).unwrap_or(u64::MAX),
                )
        })
    }

    fn tier_bytes(&self, tier: ResidentTier) -> u64 {
        let entries = self
            .entries
            .iter()
            .filter(|entry| entry.tier == tier)
            .fold(0_u64, |total, entry| {
                total.saturating_add(entry.metadata_bytes)
            });
        let payloads = self.objects.iter().filter(|object| {
            if tier == ResidentTier::Protected {
                object.protected_owners != 0
            } else {
                object.protected_owners == 0
            }
        });
        payloads.fold(entries, |total, object| {
            total.saturating_add(object.byte_length)
        })
    }

    fn dynamic_budget(&self) -> u64 {
        self.byte_budget
            .saturating_sub(self.fixed_bytes())
            .saturating_sub(self.ghost_bytes())
    }

    fn probation_limit(&self) -> u64 {
        self.dynamic_budget().saturating_mul(PROBATION_PERCENT) / 100
    }

    fn protected_limit(&self) -> u64 {
        self.dynamic_budget().saturating_sub(self.probation_limit())
    }

    pub(super) fn observe(&mut self, digest: [u8; 32]) -> u8 {
        self.observations = self.observations.saturating_add(1);
        let mut estimate = u8::MAX;
        for row in 0..SKETCH_ROWS {
            let offset = row * 2;
            let cell = usize::from(u16::from_be_bytes([digest[offset], digest[offset + 1]]))
                % SKETCH_WIDTH;
            let counter = &mut self.sketch[row][cell];
            *counter = counter.saturating_add(1).min(15);
            estimate = estimate.min(*counter);
        }
        if self.observations % SKETCH_AGE_AFTER == 0 {
            for row in &mut self.sketch {
                for counter in row {
                    *counter >>= 1;
                }
            }
            self.sketch_ages = self.sketch_ages.saturating_add(1);
        }
        estimate
    }

    fn estimated_frequency(&self, digest: [u8; 32]) -> u8 {
        let mut estimate = u8::MAX;
        for row in 0..SKETCH_ROWS {
            let offset = row * 2;
            let cell = usize::from(u16::from_be_bytes([digest[offset], digest[offset + 1]]))
                % SKETCH_WIDTH;
            estimate = estimate.min(self.sketch[row][cell]);
        }
        estimate
    }

    pub(super) fn exact_entry_index(
        &self,
        target: &crate::SemanticTargetKey,
        commit: crate::HistoryCommitId,
        roots: crate::HistoryTypedV2RootClaim,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry.key.target == *target
                && entry.key.commit == commit
                && entry.key.roots == roots
                && entry.key.tier == tier
                && entry.key.jumbo_limits == jumbo_limits
        })
    }

    pub(super) fn access(&mut self, index: usize, frequency: u8) {
        let mut promote_bytes = None;
        if let Some(entry) = self.entries.get_mut(index) {
            entry.last_access = self.observations;
            entry.recent_hits = entry.recent_hits.saturating_add(1);
            if entry.tier == ResidentTier::Probation && entry.recent_hits >= 2 {
                promote_bytes = Some(entry.metadata_bytes);
            }
        }
        if promote_bytes.is_some() {
            self.promote(index, frequency);
        }
    }

    fn promote(&mut self, index: usize, frequency: u8) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let key = entry.key.clone();
        let promotion_delta = self.entry_protected_delta(entry);
        if self
            .tier_bytes(ResidentTier::Protected)
            .saturating_add(promotion_delta)
            > self.protected_limit()
        {
            while self
                .tier_bytes(ResidentTier::Protected)
                .saturating_add(promotion_delta)
                > self.protected_limit()
            {
                let Some(victim) = self.oldest_tier(ResidentTier::Protected) else {
                    return;
                };
                if frequency <= self.estimated_frequency(self.entries[victim].digest) {
                    return;
                }
                self.remove_entry(victim);
            }
        }
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.key == key && entry.tier == ResidentTier::Probation)
        else {
            return;
        };
        for member in &self.entries[index].members {
            if let Some(pool_index) = self.object_index(member.id) {
                self.objects[pool_index].protected_owners =
                    self.objects[pool_index].protected_owners.saturating_add(1);
            }
        }
        self.entries[index].tier = ResidentTier::Protected;
    }

    fn entry_protected_delta(&self, entry: &TypedV2ResidentEntry) -> u64 {
        let mut delta = entry.metadata_bytes;
        for member in &entry.members {
            if let Some(index) = self.object_index(member.id)
                && self.objects[index].protected_owners == 0
            {
                delta = delta.saturating_add(member.byte_length);
            }
        }
        delta
    }

    fn oldest_tier(&self, tier: ResidentTier) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.tier == tier)
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(index, _)| index)
    }

    fn object_index(&self, id: ObjectId) -> Option<usize> {
        self.objects
            .binary_search_by(|candidate| candidate.id.as_bytes().cmp(id.as_bytes()))
            .ok()
    }

    pub(super) fn object_bytes(
        objects: &[TypedV2ResidentObject],
        id: ObjectId,
        schema: SchemaIdentity,
        byte_length: u64,
    ) -> Result<Option<&[u8]>, String> {
        let index = match objects
            .binary_search_by(|candidate| candidate.id.as_bytes().cmp(id.as_bytes()))
        {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let object = objects
            .get(index)
            .ok_or_else(|| "typed V2 resident object index disappeared".to_owned())?;
        if object.schema != schema || object.byte_length != byte_length {
            return Err("one object ID has conflicting resident schema or length".to_owned());
        }
        Ok(Some(&object.payload))
    }

    pub(super) fn object_claim(
        objects: &[TypedV2ResidentObject],
        id: UntrustedObjectId,
        schema: SchemaIdentity,
        byte_length: u64,
    ) -> Result<Option<(ObjectId, &[u8])>, String> {
        let index = match objects
            .binary_search_by(|candidate| candidate.id.as_bytes().cmp(id.as_bytes()))
        {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        let object = objects
            .get(index)
            .ok_or_else(|| "typed V2 resident object index disappeared".to_owned())?;
        if object.schema != schema || object.byte_length != byte_length {
            return Err("one object ID has conflicting resident schema or length".to_owned());
        }
        Ok(Some((object.id, &object.payload)))
    }

    pub(super) fn record_cold_miss(&mut self, key: &TypedV2ResidentKey) -> u8 {
        if let Some(index) = self.ghosts.iter().position(|ghost| ghost.key == *key) {
            let ghost = &mut self.ghosts[index];
            ghost.cold_misses = ghost.cold_misses.saturating_add(1);
            ghost.last_access = self.observations;
            return ghost.cold_misses;
        }
        let new_bytes = target_bytes(&key.target);
        if self.ghosts.len() >= self.max_entries {
            if let Some(victim) = self
                .ghosts
                .iter()
                .enumerate()
                .min_by_key(|(_, ghost)| ghost.last_access)
                .map(|(index, _)| index)
            {
                self.ghosts.remove(victim);
            }
        }
        while self.accounted_bytes().saturating_add(new_bytes) > self.byte_budget {
            if let Some(victim) = self.oldest_tier(ResidentTier::Probation) {
                self.remove_entry(victim);
            } else {
                return 1;
            }
        }
        self.ghosts.push(TypedV2ResidentGhost {
            key: key.clone(),
            cold_misses: 1,
            last_access: self.observations,
        });
        1
    }

    /// Adds a proof and its per-generation object map after full V2
    /// verification. New payloads are copied from the disk spool only after
    /// replacement has made room; shared pool entries are compared against
    /// the verifier spool before they acquire another generation owner.
    pub(super) fn admit(
        &mut self,
        key: TypedV2ResidentKey,
        proof: VerifiedTypedPlaneContentV2,
        manifest_bytes: &[u8],
        lineage_edge_set: Option<&[u8]>,
        segments: &[HistoryTypedV2SegmentObject],
        jumbo: &[HistoryTypedV2JumboObject],
        spool: &mut TypedV2HistorySpool,
        digest: [u8; 32],
        repeated_cold_miss: bool,
        frequency: u8,
    ) -> Result<TypedV2Admission, String> {
        let member_bytes = capacity_bytes::<TypedV2SpoolMember>(spool.members.capacity());
        let metadata_bytes = target_bytes(&key.target)
            .saturating_add(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX))
            .saturating_add(
                u64::try_from(lineage_edge_set.map_or(0, <[u8]>::len)).unwrap_or(u64::MAX),
            )
            .saturating_add(member_bytes)
            .saturating_add(capacity_bytes::<HistoryTypedV2SegmentObject>(
                segments.len(),
            ))
            .saturating_add(capacity_bytes::<HistoryTypedV2JumboObject>(jumbo.len()));
        let (mut additional_payload_bytes, mut additional_objects) =
            self.candidate_new_objects(spool)?;
        let ghost_bytes_freed = self
            .ghosts
            .iter()
            .find(|ghost| ghost.key == key)
            .map(|_| target_bytes(&key.target))
            .unwrap_or(0);
        let generation_cost = metadata_bytes.saturating_add(additional_payload_bytes);
        let desired_tier = if repeated_cold_miss && generation_cost <= self.protected_limit() {
            ResidentTier::Protected
        } else {
            ResidentTier::Probation
        };
        let tier_limit = match desired_tier {
            ResidentTier::Probation => self.probation_limit(),
            ResidentTier::Protected => self.protected_limit(),
        };
        if generation_cost > tier_limit {
            self.admission_rejections = self.admission_rejections.saturating_add(1);
            return Ok(TypedV2Admission::Rejected(proof));
        }

        loop {
            // Evicting an entry can remove pool objects that the incoming
            // generation previously shared. Recompute after every victim so
            // admission remains correctly bounded under deduplication.
            (additional_payload_bytes, additional_objects) = self.candidate_new_objects(spool)?;
            let object_capacity = self
                .objects
                .len()
                .checked_add(additional_objects)
                .ok_or_else(|| "typed V2 resident object count overflows".to_owned())?;
            let extra_table_bytes = object_capacity
                .saturating_sub(self.objects.capacity())
                .saturating_mul(size_of::<TypedV2ResidentObject>());
            let full_by_count = self.entries.len() >= self.max_entries;
            let projected_global = self
                .accounted_bytes()
                .saturating_sub(ghost_bytes_freed)
                .saturating_add(metadata_bytes)
                .saturating_add(additional_payload_bytes)
                .saturating_add(extra_table_bytes);
            let projected_tier = self.projected_tier_bytes(desired_tier, metadata_bytes, spool)?;
            if !full_by_count
                && projected_global <= self.byte_budget
                && projected_tier <= tier_limit
            {
                break;
            }
            let victim = if projected_tier > tier_limit {
                match desired_tier {
                    ResidentTier::Probation => self.oldest_tier(ResidentTier::Probation),
                    ResidentTier::Protected => self
                        .oldest_tier(ResidentTier::Probation)
                        .or_else(|| self.oldest_tier(ResidentTier::Protected)),
                }
            } else if projected_global > self.byte_budget || full_by_count {
                self.oldest_tier(ResidentTier::Probation)
                    .or_else(|| self.oldest_tier(ResidentTier::Protected))
            } else {
                None
            };
            let Some(victim) = victim else {
                self.admission_rejections = self.admission_rejections.saturating_add(1);
                return Ok(TypedV2Admission::Rejected(proof));
            };
            if self.entries[victim].tier == ResidentTier::Protected
                && !frequency_outweighs(
                    frequency,
                    self.estimated_frequency(self.entries[victim].digest),
                )
            {
                self.admission_rejections = self.admission_rejections.saturating_add(1);
                return Ok(TypedV2Admission::Rejected(proof));
            }
            self.remove_entry(victim);
        }

        if additional_objects != 0 {
            self.objects
                .try_reserve_exact(additional_objects)
                .map_err(|_| "typed V2 resident object-pool allocation failed".to_owned())?;
        }
        if self
            .accounted_bytes()
            .saturating_add(metadata_bytes)
            .saturating_add(additional_payload_bytes)
            .saturating_sub(ghost_bytes_freed)
            > self.byte_budget
        {
            self.admission_rejections = self.admission_rejections.saturating_add(1);
            return Ok(TypedV2Admission::Rejected(proof));
        }

        let pool_before = self.objects.len();
        let maximum_copied_object = spool
            .members
            .iter()
            .filter(|member| self.object_index(member.id).is_none())
            .map(|member| member.byte_length)
            .max()
            .unwrap_or(0);
        self.note_transient(
            self.accounted_bytes()
                .saturating_add(spool.transient_working_bytes())
                .saturating_add(metadata_bytes)
                .saturating_add(additional_payload_bytes)
                // Include one per-object staging allocation while bytes move
                // from the spool into their exact-size owned pool buffer.
                .saturating_add(maximum_copied_object.saturating_mul(2)),
        );
        let mut copied_payload_bytes = 0_u64;
        let mut copied_object_count = 0_usize;
        let copy_result = (|| -> Result<(), String> {
            for member in &spool.members {
                if Self::object_bytes(&self.objects, member.id, member.schema, member.byte_length)?
                    .is_some()
                {
                    self.shared_object_reuses = self.shared_object_reuses.saturating_add(1);
                    continue;
                }
                let payload = spool.read_member_payload(*member)?;
                let insertion = self.objects.binary_search_by(|candidate| {
                    candidate.id.as_bytes().cmp(member.id.as_bytes())
                });
                let index = insertion.unwrap_err();
                self.objects.insert(
                    index,
                    TypedV2ResidentObject {
                        id: member.id,
                        schema: member.schema,
                        byte_length: member.byte_length,
                        payload,
                        owners: 0,
                        protected_owners: 0,
                    },
                );
                copied_payload_bytes = copied_payload_bytes.saturating_add(member.byte_length);
                copied_object_count = copied_object_count.saturating_add(1);
                self.note_transient(
                    spool
                        .transient_working_bytes()
                        .saturating_add(self.accounted_bytes()),
                );
            }
            Ok(())
        })();
        if let Err(error) = copy_result {
            self.remove_unowned_objects();
            return Err(error);
        }
        if self.objects.len().saturating_sub(pool_before) != additional_objects
            || copied_object_count != additional_objects
            || copied_payload_bytes != additional_payload_bytes
        {
            self.remove_unowned_objects();
            return Err("typed V2 resident object pool changed during admission".to_owned());
        }

        let ghost_index = self.ghosts.iter().position(|ghost| ghost.key == key);
        if let Some(ghost_index) = ghost_index {
            self.ghosts.remove(ghost_index);
        }
        let members = std::mem::take(&mut spool.members).into_boxed_slice();
        let manifest_bytes = manifest_bytes.to_vec().into_boxed_slice();
        for member in &members {
            let index = self
                .object_index(member.id)
                .ok_or_else(|| "typed V2 resident object was not retained".to_owned())?;
            let object = &mut self.objects[index];
            if object.schema != member.schema || object.byte_length != member.byte_length {
                self.remove_unowned_objects();
                return Err(
                    "typed V2 generation member differs from pooled object identity".to_owned(),
                );
            }
            object.owners = object.owners.saturating_add(1);
            if desired_tier == ResidentTier::Protected {
                object.protected_owners = object.protected_owners.saturating_add(1);
            }
        }
        self.entries.push(TypedV2ResidentEntry {
            key,
            digest,
            proof,
            manifest_bytes,
            lineage_edge_set: lineage_edge_set.map(|bytes| bytes.to_vec().into_boxed_slice()),
            members,
            segments: segments.to_vec().into_boxed_slice(),
            jumbo: jumbo.to_vec().into_boxed_slice(),
            metadata_bytes,
            recent_hits: 1,
            last_access: self.observations,
            tier: desired_tier,
        });
        self.admissions = self.admissions.saturating_add(1);
        self.note_transient(
            spool
                .transient_working_bytes()
                .saturating_add(self.accounted_bytes()),
        );
        if self.accounted_bytes() > self.byte_budget {
            return Err("typed V2 resident admission exceeded its byte budget".to_owned());
        }
        Ok(TypedV2Admission::Resident(self.entries.len() - 1))
    }

    fn projected_tier_bytes(
        &self,
        desired_tier: ResidentTier,
        metadata_bytes: u64,
        spool: &TypedV2HistorySpool,
    ) -> Result<u64, String> {
        let mut total = self.tier_bytes(desired_tier).saturating_add(metadata_bytes);
        for member in &spool.members {
            let Some(index) = self.object_index(member.id) else {
                if desired_tier == ResidentTier::Probation {
                    total = total.saturating_add(member.byte_length);
                } else {
                    total = total.saturating_add(member.byte_length);
                }
                continue;
            };
            let object = &self.objects[index];
            if desired_tier == ResidentTier::Protected
                && object.protected_owners == 0
                && object.owners != 0
            {
                // Shared probation bytes move to the protected pool when the
                // incoming protected generation becomes an owner.
                total = total.saturating_add(object.byte_length);
            }
            if object.schema != member.schema || object.byte_length != member.byte_length {
                return Err("one object ID has conflicting resident schema or length".to_owned());
            }
        }
        Ok(total)
    }

    fn candidate_new_objects(&self, spool: &TypedV2HistorySpool) -> Result<(u64, usize), String> {
        let mut bytes = 0_u64;
        let mut count = 0_usize;
        for member in &spool.members {
            match Self::object_bytes(&self.objects, member.id, member.schema, member.byte_length)? {
                Some(existing) => {
                    if !spool.member_payload_equals(*member, existing)? {
                        return Err(
                            "verified spool bytes differ from a resident object ID".to_owned()
                        );
                    }
                }
                None => {
                    bytes = bytes.checked_add(member.byte_length).ok_or_else(|| {
                        "typed V2 resident payload accounting overflows".to_owned()
                    })?;
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| "typed V2 resident object count overflows".to_owned())?;
                }
            }
        }
        Ok((bytes, count))
    }

    fn remove_entry(&mut self, index: usize) {
        if index >= self.entries.len() {
            return;
        }
        let entry = self.entries.remove(index);
        for member in &entry.members {
            if let Some(pool_index) = self.object_index(member.id) {
                let object = &mut self.objects[pool_index];
                object.owners = object.owners.saturating_sub(1);
                if entry.tier == ResidentTier::Protected {
                    object.protected_owners = object.protected_owners.saturating_sub(1);
                }
            }
        }
        self.remove_unowned_objects();
        self.evictions = self.evictions.saturating_add(1);
    }

    fn remove_unowned_objects(&mut self) {
        self.objects.retain(|object| object.owners != 0);
    }

    fn note_transient(&mut self, bytes: u64) {
        self.transient_high_water_bytes = self.transient_high_water_bytes.max(bytes);
    }

    pub(super) fn note_spool_work(&mut self, spool: &TypedV2HistorySpool) {
        self.note_transient(
            self.accounted_bytes()
                .saturating_add(spool.transient_working_bytes()),
        );
    }
}

pub(super) fn typed_v2_resident_key_digest(
    target: &crate::SemanticTargetKey,
    commit: crate::HistoryCommitId,
    roots: crate::HistoryTypedV2RootClaim,
    tier: SemanticTypedPlaneVerificationTierV2,
    jumbo_limits: JumboRopeLimits,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.typed-v2-live-residency-key.v2\0");
    for field in [target.package().as_bytes(), target.coordinate().as_bytes()] {
        hasher.update(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(field);
    }
    hasher.update(&<[u8; 2]>::from(target.profile()));
    hasher.update(commit.as_bytes());
    hasher.update(roots.content_root_claim().as_bytes());
    hasher.update(roots.generation_root_claim().as_bytes());
    hasher.update(roots.closure().as_bytes());
    hasher.update(roots.locator().as_bytes());
    hasher.update(&[match tier {
        SemanticTypedPlaneVerificationTierV2::Standard => 0,
        SemanticTypedPlaneVerificationTierV2::LargePackage => 1,
    }]);
    hasher.update(&jumbo_limits.max_value_bytes.to_be_bytes());
    hasher.update(&jumbo_limits.max_leaf_count.to_be_bytes());
    hasher.update(
        &u64::try_from(jumbo_limits.max_metadata_bytes)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    *hasher.finalize().as_bytes()
}

fn target_bytes(target: &crate::SemanticTargetKey) -> u64 {
    u64::try_from(target.package().len())
        .unwrap_or(u64::MAX)
        .saturating_add(u64::try_from(target.coordinate().len()).unwrap_or(u64::MAX))
}

fn capacity_bytes<T>(capacity: usize) -> u64 {
    u64::try_from(capacity)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(size_of::<T>()).unwrap_or(u64::MAX))
}

fn frequency_outweighs(candidate: u8, victim: u8) -> bool {
    candidate > victim
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn trace_digest(id: u64) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        for row in 0..SKETCH_ROWS {
            let multiplier = 31_u64 + u64::try_from(row).expect("row fits u64") * 2;
            let offset = 17_u64 + u64::try_from(row).expect("row fits u64") * 97;
            let cell = u16::try_from(
                id.wrapping_mul(multiplier).wrapping_add(offset)
                    % u64::try_from(SKETCH_WIDTH).expect("sketch width fits u64"),
            )
            .expect("cell fits u16");
            digest[row * 2..row * 2 + 2].copy_from_slice(&cell.to_be_bytes());
        }
        digest
    }

    #[test]
    fn repeated_full_scan_stays_below_repeated_hot_access_under_lru_ab() {
        const HOT_KEYS: [u64; 2] = [1, 2];
        const SCAN_KEYS: u64 = 24;
        const LRU_CAPACITY: usize = 4;
        const HOT_REPEATS: usize = 64;

        let mut lru = VecDeque::new();
        for _ in 0..HOT_REPEATS {
            for key in HOT_KEYS {
                if let Some(position) = lru.iter().position(|seen| *seen == key) {
                    lru.remove(position);
                }
                lru.push_back(key);
                while lru.len() > LRU_CAPACITY {
                    lru.pop_front();
                }
            }
        }
        for _pass in 0..2 {
            for key in 10..10 + SCAN_KEYS {
                if let Some(position) = lru.iter().position(|seen| *seen == key) {
                    lru.remove(position);
                }
                lru.push_back(key);
                while lru.len() > LRU_CAPACITY {
                    lru.pop_front();
                }
            }
        }
        let mut cache = TypedV2HistoryResidencyCache::new(64 * 1024, LRU_CAPACITY)
            .expect("construct bounded deterministic frequency trace");
        let hot_scores = HOT_KEYS.map(|key| {
            let digest = trace_digest(key);
            for _ in 0..HOT_REPEATS {
                cache.observe(digest);
            }
            cache.estimated_frequency(digest)
        });
        for _pass in 0..2 {
            for key in 10..10 + SCAN_KEYS {
                cache.observe(trace_digest(key));
            }
        }
        let scan_score = cache.estimated_frequency(trace_digest(10 + SCAN_KEYS - 1));
        assert!(HOT_KEYS.iter().all(|key| !lru.contains(key)));
        assert!(hot_scores.iter().all(|score| *score > scan_score));
        assert!(
            hot_scores
                .iter()
                .all(|score| frequency_outweighs(scan_score, *score) == false)
        );
    }
}
