use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use backend_semantic::ir::{
    GenerationId, LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticInputWitness,
    SemanticIrPlane, SemanticManifestRoot, SemanticPlane, SemanticPlaneCatalog,
    SemanticPlaneCatalogEntry, SemanticPlaneCoverageScope, SemanticPlaneImageKey,
    SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneSegment, SemanticRangeRequest,
    SemanticSegmentId,
};
use backend_semantic::vocabulary::Stage;
use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, Coverage, CoverageAdmissionError,
    CoverageWitness, ObjectVersion, ProducerObservationClaims, ProducerObservationVerifier, Schema,
    ScopeRoot, UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
};

use crate::{
    ByteRange, DurableSemanticRangeStore, DurableSemanticSegmentStore, IrResidencyCasReason,
    IrResidencyDeltaHop, IrResidencyError, IrResidencyLimits, IrResidencyPath,
    SelectedGenerationSource, SelectedGenerationStamp, SelectedSemanticPlane, SparseCoverage,
};

use super::AdaptiveIrResidency;

struct TestAuthority;

impl Schema for TestAuthority {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 0xfffe;
    type Value = [u8; 32];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

struct TestVerifier;

impl ProducerObservationVerifier for TestVerifier {
    type Error = CoverageAdmissionError;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

fn complete_witness<T: Schema>(version: ObjectVersion<T>) -> CoverageWitness {
    let claim = AuthorityScopeClaim::from_object_version(version);
    let producer: AdmittedProducerObservation = admit_producer_observation(
        UntrustedProducerObservation::new([7; 32], claim.scope_root(), [8; 32], vec![9, 10]),
        &TestVerifier,
    )
    .expect("test producer observation is admitted");
    CoverageWitness::Complete(admit_complete_scope(claim, producer).expect("scope matches"))
}

fn input() -> SemanticInputWitness {
    let scope_value = [12; 32];
    let version = ObjectVersion::<TestAuthority>::from_value(&scope_value);
    let scope = ScopeRoot::from_bytes(version.to_bytes());
    SemanticInputWitness::admitted([11; 32], scope, complete_witness(version))
        .expect("input witness is admitted")
}

fn build() -> SemanticBuildIdentity {
    SemanticBuildIdentity::new(
        [1; 32],
        [2; 32],
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        [3; 32],
        [4; 32],
        [5; 32],
        [6; 32],
    )
}

fn manifest(generation: u8, payload: &[u8]) -> SemanticPlaneManifest {
    manifest_for_kind(generation, payload, SemanticIrPlane::Core)
}

fn manifest_for_kind(
    generation: u8,
    payload: &[u8],
    ir_plane: SemanticIrPlane,
) -> SemanticPlaneManifest {
    let kind = SemanticPlaneKind::Ir(ir_plane);
    let input = input();
    let segment =
        SemanticPlaneSegment::from_payload_with_witness(kind, [4; 32], [4; 32], 1, payload, input)
            .expect("segment identity and witness");
    let claimed =
        SemanticPlane::claimed(kind, vec![segment], Coverage::Complete).expect("plane claim");
    let witness = complete_witness(ObjectVersion::<SemanticPlaneCoverageScope>::from_value(
        &claimed.root(),
    ));
    let plane =
        SemanticPlane::admitted(kind, claimed.segments().to_vec(), witness).expect("plane witness");
    SemanticPlaneManifest::new(
        GenerationId::from_raw([generation; 32]),
        build(),
        input,
        vec![plane],
    )
    .expect("semantic manifest")
}

fn manifest_with_segments(generation: u8, payloads: &[&[u8]]) -> SemanticPlaneManifest {
    let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
    let input = input();
    let segments = payloads
        .iter()
        .enumerate()
        .map(|(index, payload)| {
            let first = u8::try_from(index * 2).expect("small fixture index");
            let last = first.checked_add(1).expect("small fixture range");
            SemanticPlaneSegment::from_payload_with_witness(
                kind,
                [first; 32],
                [last; 32],
                1,
                payload,
                input,
            )
            .expect("segment identity and witness")
        })
        .collect::<Vec<_>>();
    let claimed = SemanticPlane::claimed(kind, segments, Coverage::Complete).expect("plane claim");
    let witness = complete_witness(ObjectVersion::<SemanticPlaneCoverageScope>::from_value(
        &claimed.root(),
    ));
    let plane =
        SemanticPlane::admitted(kind, claimed.segments().to_vec(), witness).expect("plane witness");
    SemanticPlaneManifest::new(
        GenerationId::from_raw([generation; 32]),
        build(),
        input,
        vec![plane],
    )
    .expect("semantic manifest")
}

fn selection_for(
    manifest: &SemanticPlaneManifest,
    revision: u64,
) -> (SelectionSource, SelectedSemanticPlane) {
    selection_for_kind(manifest, revision, SemanticIrPlane::Core)
}

fn selection_for_kind(
    manifest: &SemanticPlaneManifest,
    revision: u64,
    ir_plane: SemanticIrPlane,
) -> (SelectionSource, SelectedSemanticPlane) {
    let image = SemanticPlaneImageKey::from_manifest(0, manifest);
    let manifest_bytes = manifest.encode().expect("manifest encoding");
    let entry = SemanticPlaneCatalogEntry::new(
        image,
        u32::try_from(manifest_bytes.len()).expect("small manifest"),
    )
    .expect("catalog entry");
    let catalog = SemanticPlaneCatalog::new(vec![entry]).expect("catalog");
    let stamp = SelectedGenerationStamp::checked(
        [1; 16],
        manifest.build().profile(),
        [2; 32],
        revision,
        [3; 32],
        [4; 32],
        catalog.root(),
    )
    .expect("selected generation stamp");
    let mut source = SelectionSource {
        current: stamp,
        image,
        stale_after_read: None,
    };
    let selection = SelectedSemanticPlane::select(
        &mut source,
        manifest,
        image,
        SemanticPlaneKind::Ir(ir_plane),
    )
    .expect("selected semantic plane");
    (source, selection)
}

#[derive(Clone)]
struct SelectionSource {
    current: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    stale_after_read: Option<Arc<OnceLock<SelectedGenerationStamp>>>,
}

impl SelectionSource {
    fn observed(&self) -> SelectedGenerationStamp {
        self.stale_after_read
            .as_ref()
            .and_then(|stamp| stamp.get().copied())
            .unwrap_or(self.current)
    }
}

impl SelectedGenerationSource for SelectionSource {
    type Error = &'static str;

    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
        Ok(self.observed())
    }

    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error> {
        Ok(expected_stamp == self.observed() && image == self.image)
    }
}

#[derive(Default)]
struct MemoryRangeStore {
    objects: HashMap<([u8; 32], [u8; 32]), Box<[u8]>>,
    corrupt_roots: HashSet<[u8; 32]>,
    unreadable_roots: HashSet<[u8; 32]>,
    delay_roots: HashMap<[u8; 32], Duration>,
    reads: usize,
    flip_on_read: Option<(
        Arc<OnceLock<SelectedGenerationStamp>>,
        SelectedGenerationStamp,
    )>,
}

impl MemoryRangeStore {
    fn insert(&mut self, selection: SelectedSemanticPlane, id: SemanticSegmentId, payload: &[u8]) {
        self.objects.insert(
            (
                *selection.image().manifest_root().as_bytes(),
                *id.as_bytes(),
            ),
            payload.into(),
        );
    }

    fn corrupt(&mut self, selection: SelectedSemanticPlane) {
        self.corrupt_roots
            .insert(*selection.image().manifest_root().as_bytes());
    }
}

impl DurableSemanticSegmentStore for MemoryRangeStore {
    type Error = &'static str;

    fn commit_and_read(
        &mut self,
        _selection: SelectedSemanticPlane,
        _segment: SemanticSegmentId,
        payload: &[u8],
        admit: &mut dyn FnMut(&[u8]) -> Result<(), crate::ReplicationError>,
    ) -> Result<Box<[u8]>, Self::Error> {
        admit(payload).map_err(|_| "payload admission failed")?;
        Ok(payload.into())
    }
}

impl DurableSemanticRangeStore for MemoryRangeStore {
    type RangeError = &'static str;

    fn stage_durable_range(
        &mut self,
        _selection: SelectedSemanticPlane,
        _request: SemanticRangeRequest,
        _byte_range: ByteRange,
        _payload: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError> {
        Err("not used by residency tests")
    }

    fn read_complete_segment(
        &mut self,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Option<Box<[u8]>>, Self::RangeError> {
        self.reads += 1;
        if let Some((current, stale)) = &self.flip_on_read {
            let _ = current.set(*stale);
        }
        let root = *selection.image().manifest_root().as_bytes();
        if let Some(delay) = self.delay_roots.get(&root) {
            std::thread::sleep(*delay);
        }
        if self.unreadable_roots.contains(&root) {
            return Err("historical CAS entry is unreadable");
        }
        let Some(bytes) = self.objects.get(&(root, *request.segment_id.as_bytes())) else {
            return Ok(None);
        };
        let mut bytes = bytes.to_vec().into_boxed_slice();
        if self.corrupt_roots.contains(&root) && !bytes.is_empty() {
            bytes[0] ^= 1;
        }
        Ok(Some(bytes))
    }

    fn checkpoint_sparse_segment(
        &mut self,
        _selection: SelectedSemanticPlane,
        _request: SemanticRangeRequest,
    ) -> Result<Vec<u8>, Self::RangeError> {
        Err("not used by residency tests")
    }

    fn resume_sparse_segment(
        &mut self,
        _selection: SelectedSemanticPlane,
        _request: SemanticRangeRequest,
        _checkpoint: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError> {
        Err("not used by residency tests")
    }

    fn discard_sparse_segment(
        &mut self,
        _selection: SelectedSemanticPlane,
        _request: SemanticRangeRequest,
    ) -> Result<(), Self::RangeError> {
        Err("not used by residency tests")
    }
}

fn limits() -> IrResidencyLimits {
    IrResidencyLimits {
        hot_bytes: 1024,
        pinned_bytes: 1024,
        entries: 8,
        warm_uses: 2,
        warm_cold_cost: Duration::ZERO,
        delta_hops: 3,
        delta_changed_bytes: 1024,
        delta_actions: 100,
    }
}

#[test]
fn stale_selection_is_denied_before_cas_or_borrowed_exposure() {
    let manifest = manifest(1, b"segment");
    let (mut source, selection) = selection_for(&manifest, 1);
    source.current = selection_for(&manifest, 2).0.current;
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let mut store = MemoryRangeStore::default();
    let mut cache = AdaptiveIrResidency::new(limits());
    assert!(matches!(
        cache.plan(&mut source, selection, &manifest, segment, &[]),
        Err(IrResidencyError::StaleSelection)
    ));
    let result = cache.with_segment(
        &mut source,
        selection,
        &manifest,
        segment,
        &[],
        &mut store,
        |_| panic!("stale bytes must not be exposed"),
    );
    assert!(matches!(result, Err(IrResidencyError::StaleSelection)));
    assert_eq!(store.reads, 0);
}

#[test]
fn selection_that_changes_during_a_cas_read_is_denied_afterward() {
    let manifest = manifest(1, b"segment");
    let (mut source, selection) = selection_for(&manifest, 1);
    let stale_stamp = selection_for(&manifest, 2).0.current;
    let stale_after_read = Arc::new(OnceLock::new());
    source.stale_after_read = Some(Arc::clone(&stale_after_read));
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let mut store = MemoryRangeStore::default();
    store.insert(
        selection,
        segment.admitted_id().expect("admitted"),
        b"segment",
    );
    store.flip_on_read = Some((stale_after_read, stale_stamp));
    let mut cache = AdaptiveIrResidency::new(limits());
    let result = cache.with_segment(
        &mut source,
        selection,
        &manifest,
        segment,
        &[],
        &mut store,
        |_| panic!("stale CAS bytes must not be exposed"),
    );
    assert!(matches!(result, Err(IrResidencyError::StaleSelection)));
    assert_eq!(store.reads, 1);
}

#[test]
fn mismatched_manifest_root_is_denied_before_cas() {
    let selected = manifest(1, b"selected");
    let other = manifest(2, b"other");
    let (mut source, selection) = selection_for(&selected, 1);
    let other_segment = &other.plane(selection.kind()).expect("plane").segments()[0];
    let mut store = MemoryRangeStore::default();
    let mut cache = AdaptiveIrResidency::new(limits());
    let result = cache.with_segment(
        &mut source,
        selection,
        &other,
        other_segment,
        &[],
        &mut store,
        |_| panic!("unselected root bytes must not be exposed"),
    );
    assert!(matches!(
        result,
        Err(IrResidencyError::SelectionManifestMismatch)
    ));
    assert_eq!(store.reads, 0);
}

#[test]
fn fresh_owner_binding_can_reuse_the_same_hot_content_identity() {
    let first = manifest(1, b"shared segment");
    let second = manifest(2, b"shared segment");
    let (mut first_source, first_selection) = selection_for(&first, 1);
    let (mut second_source, second_selection) = selection_for(&second, 2);
    let first_segment = &first
        .plane(first_selection.kind())
        .expect("plane")
        .segments()[0];
    let second_segment = &second
        .plane(second_selection.kind())
        .expect("plane")
        .segments()[0];
    assert_eq!(
        first_segment.id_claim().as_bytes(),
        second_segment.id_claim().as_bytes()
    );

    let mut store = MemoryRangeStore::default();
    let id = first_segment
        .admitted_id()
        .expect("fixture segment admitted");
    store.insert(first_selection, id, b"shared segment");
    store.insert(second_selection, id, b"shared segment");
    let mut cache = AdaptiveIrResidency::new(limits());
    for _ in 0..2 {
        let (path, bytes) = cache
            .with_segment(
                &mut first_source,
                first_selection,
                &first,
                first_segment,
                &[],
                &mut store,
                |bytes| bytes.to_vec(),
            )
            .expect("cold segment read");
        assert!(matches!(path, IrResidencyPath::PristineCas(_)));
        assert_eq!(bytes, b"shared segment");
    }
    let (path, bytes) = cache
        .with_segment(
            &mut second_source,
            second_selection,
            &second,
            second_segment,
            &[],
            &mut store,
            |bytes| bytes.to_vec(),
        )
        .expect("fresh target reuses hot bytes");
    assert_eq!(path, IrResidencyPath::HotMemory);
    assert_eq!(bytes, b"shared segment");
    assert_eq!(store.reads, 2);
}

#[test]
fn invalid_hot_owner_metadata_is_evicted_before_bytes_are_borrowed() {
    let wrong_manifest = manifest(2, b"wrong");
    let manifest = manifest(1, b"expected");
    let (mut source, selection) = selection_for(&manifest, 1);
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let wrong_id = wrong_manifest
        .plane(selection.kind())
        .expect("wrong plane")
        .segments()[0]
        .admitted_id()
        .expect("wrong segment admitted");
    let mut policy = limits();
    policy.warm_uses = 1;
    let mut cache = AdaptiveIrResidency::new(policy);
    let mut store = MemoryRangeStore::default();
    store.insert(
        selection,
        segment.admitted_id().expect("segment admitted"),
        b"expected",
    );
    cache
        .with_segment(
            &mut source,
            selection,
            &manifest,
            segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("segment becomes hot");

    let key = super::SegmentLookupKey::from_descriptor(selection.kind(), segment);
    let Some(entry) = cache.hot.get_mut(&key) else {
        panic!("hot entry was admitted");
    };
    let super::HotOwner::Owned(owner) = &mut entry.owner else {
        panic!("no lease has shared the owner");
    };
    owner.id = wrong_id;

    let (path, bytes) = cache
        .with_segment(
            &mut source,
            selection,
            &manifest,
            segment,
            &[],
            &mut store,
            |bytes| bytes.to_vec(),
        )
        .expect("damaged hot metadata falls back to verified CAS");
    assert!(matches!(path, IrResidencyPath::PristineCas(_)));
    assert_eq!(bytes, b"expected");
    assert_eq!(cache.metrics().corrupt_hot_owners, 1);
    assert_eq!(store.reads, 2);
}

#[test]
fn hot_admission_uses_the_weighted_observed_cold_route_mean() {
    let manifest = manifest(1, b"segment");
    let (_, selection) = selection_for(&manifest, 1);
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let key = super::SegmentLookupKey::from_descriptor(selection.kind(), segment);
    let mut policy = limits();
    policy.warm_uses = 4;
    policy.warm_cold_cost = Duration::from_nanos(75);
    let mut cache = AdaptiveIrResidency::new(policy);
    cache.observations.insert(
        key,
        super::Observation {
            uses: 4,
            scanned: super::ColdCost {
                pristine_samples: 3,
                delta_samples: 1,
                mean_pristine_ns: 100,
                mean_delta_ns: 10,
            },
            ..super::Observation::default()
        },
    );

    assert!(cache.should_warm(key));
}

#[test]
fn prepared_delta_route_learns_independently_of_per_read_action_scans() {
    let manifest = manifest(1, b"segment");
    let (_, selection) = selection_for(&manifest, 1);
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let key = super::SegmentLookupKey::from_descriptor(selection.kind(), segment);
    let mut cache = AdaptiveIrResidency::new(limits());
    cache.observations.insert(
        key,
        super::Observation {
            scanned: super::ColdCost {
                pristine_samples: 4,
                delta_samples: 4,
                mean_pristine_ns: 100,
                mean_delta_ns: 400,
            },
            ..super::Observation::default()
        },
    );
    let summary = super::IrResidencyDeltaSummary {
        hops: 1,
        actions: 1,
        reused_segments: 1,
        changed_bytes: 0,
        target_segment_reused: true,
    };
    assert_eq!(
        cache.delta_choice(
            key,
            summary,
            1,
            segment.byte_length(),
            super::RouteMode::Scanned
        ),
        super::DeltaChoice::CostHigher,
    );
    assert_eq!(
        cache.delta_choice(
            key,
            summary,
            1,
            segment.byte_length(),
            super::RouteMode::Prepared
        ),
        super::DeltaChoice::Use,
    );
}

#[test]
fn byte_budget_counts_leases_after_their_hot_entry_is_evicted() {
    let first = manifest(1, b"four");
    let second = manifest(2, b"x");
    let (mut source, first_selection) = selection_for(&first, 1);
    let (mut second_source, second_selection) = selection_for(&second, 2);
    let first_segment = &first
        .plane(first_selection.kind())
        .expect("plane")
        .segments()[0];
    let second_segment = &second
        .plane(second_selection.kind())
        .expect("plane")
        .segments()[0];
    let mut policy = limits();
    policy.hot_bytes = 5;
    policy.entries = 1;
    policy.warm_uses = 1;
    let mut cache = AdaptiveIrResidency::new(policy);
    let mut store = MemoryRangeStore::default();
    store.insert(
        first_selection,
        first_segment.admitted_id().expect("admitted"),
        b"four",
    );
    store.insert(
        second_selection,
        second_segment.admitted_id().expect("admitted"),
        b"x",
    );

    cache
        .with_segment(
            &mut source,
            first_selection,
            &first,
            first_segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("first segment becomes hot");
    let lease = cache
        .lease_hot_segment(&mut source, first_selection, &first, first_segment)
        .expect("lease lookup")
        .expect("hot lease");
    assert_eq!(cache.metrics().live_owner_bytes, 4);
    assert_eq!(cache.metrics().pinned_owner_bytes, 4);

    cache
        .with_segment(
            &mut second_source,
            second_selection,
            &second,
            second_segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("second segment replaces the hot entry");
    let measured = cache.metrics();
    assert_eq!(measured.hot_bytes, 1);
    assert_eq!(measured.live_owner_bytes, 5);
    assert_eq!(measured.pinned_owner_bytes, 4);
    assert_eq!(measured.evictions, 1);
    drop(lease);
    assert_eq!(cache.metrics().live_owner_bytes, 1);
    assert_eq!(cache.metrics().pinned_owner_bytes, 0);
}

#[test]
fn pinned_byte_limit_denies_a_new_owner_lease() {
    let manifest = manifest(1, b"four");
    let (mut source, selection) = selection_for(&manifest, 1);
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let mut policy = limits();
    policy.warm_uses = 1;
    policy.pinned_bytes = 3;
    let mut cache = AdaptiveIrResidency::new(policy);
    let mut store = MemoryRangeStore::default();
    store.insert(selection, segment.admitted_id().expect("admitted"), b"four");
    cache
        .with_segment(
            &mut source,
            selection,
            &manifest,
            segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("segment becomes hot");

    assert!(matches!(
        cache.lease_hot_segment(&mut source, selection, &manifest, segment),
        Err(IrResidencyError::PinnedBudgetExceeded)
    ));
    assert_eq!(cache.metrics().pinned_owner_bytes, 0);
}

#[test]
fn outstanding_lease_rechecks_its_exact_owner_stamp_on_each_borrow() {
    let manifest = manifest(1, b"leased");
    let (mut source, selection) = selection_for(&manifest, 1);
    let stale = selection_for(&manifest, 2).0.current;
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let mut policy = limits();
    policy.warm_uses = 1;
    let mut cache = AdaptiveIrResidency::new(policy);
    let mut store = MemoryRangeStore::default();
    store.insert(
        selection,
        segment.admitted_id().expect("admitted"),
        b"leased",
    );
    cache
        .with_segment(
            &mut source,
            selection,
            &manifest,
            segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("segment becomes hot");
    let lease = cache
        .lease_hot_segment(&mut source, selection, &manifest, segment)
        .expect("lease lookup")
        .expect("hot lease");

    source.current = stale;
    assert!(matches!(
        lease.with_payload(&mut source, |_| panic!("stale lease must not expose bytes")),
        Err(IrResidencyError::StaleSelection)
    ));
}

#[test]
fn delta_hop_and_changed_byte_caps_select_pristine_cas() {
    let base = manifest(1, b"before");
    let middle = manifest(2, b"after edit");
    let target = manifest(3, b"after edit");
    let (_, base_selection) = selection_for(&base, 1);
    let (_, middle_selection) = selection_for(&middle, 2);
    let (mut target_source, target_selection) = selection_for(&target, 3);
    let segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let hop_one = IrResidencyDeltaHop::new(&base, &middle, base_selection, base.root());
    let hop_two = IrResidencyDeltaHop::new(&middle, &target, middle_selection, middle.root());
    let chain = [hop_one, hop_two];
    let mut store = MemoryRangeStore::default();
    let mut cache = AdaptiveIrResidency::new(IrResidencyLimits {
        delta_hops: 1,
        ..limits()
    });
    assert_eq!(
        cache
            .plan(
                &mut target_source,
                target_selection,
                &target,
                segment,
                &chain
            )
            .expect("route plan"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaBoundExceeded),
    );

    let changed_target = manifest(4, b"new target payload");
    let (mut changed_source, changed_selection) = selection_for(&changed_target, 4);
    let changed_segment = &changed_target
        .plane(changed_selection.kind())
        .expect("plane")
        .segments()[0];
    let one_hop = [IrResidencyDeltaHop::new(
        &middle,
        &changed_target,
        middle_selection,
        middle.root(),
    )];
    let mut compact = AdaptiveIrResidency::new(IrResidencyLimits {
        delta_changed_bytes: 1,
        ..limits()
    });
    assert_eq!(
        compact
            .plan(
                &mut changed_source,
                changed_selection,
                &changed_target,
                changed_segment,
                &one_hop
            )
            .expect("route plan"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaBoundExceeded),
    );
}

#[test]
fn delta_route_rejects_unadmitted_claims_and_wrong_base_roots() {
    let admitted_base = manifest(1, b"same bytes");
    let admitted_target = manifest(2, b"same bytes");
    let base_bytes = admitted_base.encode().expect("base encoding");
    let target_bytes = admitted_target.encode().expect("target encoding");
    let base = SemanticPlaneManifest::decode(&base_bytes).expect("cold base manifest");
    let target = SemanticPlaneManifest::decode(&target_bytes).expect("cold target manifest");
    assert!(!base.claims_admitted());
    assert!(!target.claims_admitted());
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let mut cache = AdaptiveIrResidency::new(limits());
    let unadmitted = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    assert_eq!(
        cache
            .plan(
                &mut target_source,
                target_selection,
                &target,
                segment,
                &unadmitted
            )
            .expect("safe fallback plan"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
    );

    let (_, admitted_base_selection) = selection_for(&admitted_base, 3);
    let (mut admitted_target_source, admitted_target_selection) =
        selection_for(&admitted_target, 4);
    let wrong_root = [IrResidencyDeltaHop::new(
        &admitted_base,
        &admitted_target,
        admitted_base_selection,
        SemanticManifestRoot::from_wire_claim([99; 32]),
    )];
    let admitted_segment = &admitted_target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    assert_eq!(
        cache
            .plan(
                &mut admitted_target_source,
                admitted_target_selection,
                &admitted_target,
                admitted_segment,
                &wrong_root,
            )
            .expect("safe fallback plan"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
    );
}

#[test]
fn delta_route_rejects_a_base_binding_for_another_plane() {
    let base = manifest_for_kind(1, b"same bytes", SemanticIrPlane::Types);
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for_kind(&base, 1, SemanticIrPlane::Types);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let segment = &target
        .plane(target_selection.kind())
        .expect("target plane")
        .segments()[0];
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut cache = AdaptiveIrResidency::new(limits());
    assert_eq!(
        cache
            .plan(
                &mut target_source,
                target_selection,
                &target,
                segment,
                &chain
            )
            .expect("mismatched historical plane safely falls back"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
    );
}

#[test]
fn prepared_delta_route_matches_individual_reads_and_rejects_stale_or_malformed_use() {
    let payloads: [&[u8]; 3] = [b"first", b"second", b"third"];
    let base = manifest_with_segments(1, &payloads);
    let target = manifest_with_segments(2, &payloads);
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segments = base
        .plane(target_selection.kind())
        .expect("base plane")
        .segments();
    let target_segments = target
        .plane(target_selection.kind())
        .expect("target plane")
        .segments();
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut prepared_cache = AdaptiveIrResidency::new(limits());
    let prepared = prepared_cache.prepare_delta_route(target_selection, &target, &chain);
    let planned_actions = prepared_cache.metrics().delta_actions_scanned;
    assert_eq!(planned_actions, payloads.len() as u64);

    let mut individual_cache = AdaptiveIrResidency::new(limits());
    let mut prepared_store = MemoryRangeStore::default();
    let mut individual_store = MemoryRangeStore::default();
    for (base_segment, payload) in base_segments.iter().zip(payloads) {
        let id = base_segment.admitted_id().expect("base segment admitted");
        prepared_store.insert(base_selection, id, payload);
        individual_store.insert(base_selection, id, payload);
    }

    for (target_segment, expected) in target_segments.iter().zip(payloads) {
        let (prepared_path, prepared_bytes) = prepared_cache
            .with_segment_prepared(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &prepared,
                &mut prepared_store,
                <[u8]>::to_vec,
            )
            .expect("prepared delta read");
        let (individual_path, individual_bytes) = individual_cache
            .with_segment(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &chain,
                &mut individual_store,
                <[u8]>::to_vec,
            )
            .expect("individual delta read");
        assert_eq!(prepared_path, individual_path);
        assert_eq!(prepared_bytes, expected);
        assert_eq!(prepared_bytes, individual_bytes);
    }
    assert_eq!(
        prepared_cache.metrics().delta_actions_scanned,
        planned_actions,
        "prepared reads reuse the one validated action scan"
    );
    assert_eq!(prepared_store.reads, payloads.len());
    assert_eq!(individual_store.reads, payloads.len());

    target_source.current = selection_for(&target, 3).0.current;
    assert!(matches!(
        prepared_cache.plan_with_prepared_route(
            &mut target_source,
            target_selection,
            &target,
            &target_segments[0],
            &prepared,
        ),
        Err(IrResidencyError::StaleSelection)
    ));

    let other_target = manifest(4, b"different target");
    let (mut other_source, other_selection) = selection_for(&other_target, 4);
    let other_segment = &other_target
        .plane(other_selection.kind())
        .expect("other plane")
        .segments()[0];
    assert_eq!(
        prepared_cache
            .plan_with_prepared_route(
                &mut other_source,
                other_selection,
                &other_target,
                other_segment,
                &prepared,
            )
            .expect("route bound to another root safely falls back"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
    );

    let malformed_chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        SemanticManifestRoot::from_wire_claim([99; 32]),
    )];
    let mut malformed_cache = AdaptiveIrResidency::new(limits());
    let malformed =
        malformed_cache.prepare_delta_route(target_selection, &target, &malformed_chain);
    let (mut fresh_source, fresh_selection) = selection_for(&target, 5);
    assert_eq!(
        malformed_cache
            .plan_with_prepared_route(
                &mut fresh_source,
                fresh_selection,
                &target,
                &target_segments[0],
                &malformed,
            )
            .expect("malformed route safely falls back"),
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
    );
}

#[test]
fn corrupt_reusable_base_cas_falls_back_to_verified_target_cas() {
    let base = manifest(1, b"same bytes");
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segment = &base.plane(base_selection.kind()).expect("plane").segments()[0];
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let hop = IrResidencyDeltaHop::new(&base, &target, base_selection, base.root());
    let chain = [hop];
    let mut store = MemoryRangeStore::default();
    store.insert(
        base_selection,
        base_segment.admitted_id().expect("base admitted"),
        b"same bytes",
    );
    store.insert(
        target_selection,
        target_segment.admitted_id().expect("target admitted"),
        b"same bytes",
    );
    store.corrupt(base_selection);
    let mut cache = AdaptiveIrResidency::new(limits());
    let (path, bytes) = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            |bytes| bytes.to_vec(),
        )
        .expect("pristine target survives corrupt base object");
    assert_eq!(bytes, b"same bytes");
    assert_eq!(
        path,
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaCandidateUnavailable)
    );
    assert_eq!(store.reads, 2);
    assert_eq!(cache.metrics().corrupt_cas_objects, 1);
}

#[test]
fn unreadable_historical_cas_falls_back_to_verified_target_cas() {
    let base = manifest(1, b"same bytes");
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut store = MemoryRangeStore::default();
    store.unreadable_roots.insert(*base.root().as_bytes());
    store.insert(
        target_selection,
        target_segment.admitted_id().expect("target admitted"),
        b"same bytes",
    );
    let mut cache = AdaptiveIrResidency::new(limits());
    let (path, bytes) = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            |bytes| bytes.to_vec(),
        )
        .expect("selected target remains available after historical read error");
    assert_eq!(bytes, b"same bytes");
    assert_eq!(
        path,
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaCandidateUnavailable),
    );
    assert_eq!(store.reads, 2);
    assert_eq!(cache.metrics().unreadable_delta_candidates, 1);
}

#[test]
fn checked_delta_route_reads_exact_reused_segment_from_base_cas() {
    let base = manifest(1, b"same bytes");
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segment = &base.plane(base_selection.kind()).expect("plane").segments()[0];
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut store = MemoryRangeStore::default();
    store.insert(
        base_selection,
        base_segment.admitted_id().expect("base admitted"),
        b"same bytes",
    );
    let mut cache = AdaptiveIrResidency::new(limits());
    let (path, bytes) = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            |bytes| bytes.to_vec(),
        )
        .expect("checked base reuse");
    assert!(matches!(path, IrResidencyPath::DeltaCas(_)));
    assert_eq!(bytes, b"same bytes");
    assert_eq!(store.reads, 1);
}

#[test]
fn measured_delta_cost_uses_a_pristine_probe_and_hysteresis() {
    let base = manifest(1, b"same bytes");
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segment = &base.plane(base_selection.kind()).expect("plane").segments()[0];
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut store = MemoryRangeStore::default();
    store.insert(
        base_selection,
        base_segment.admitted_id().expect("admitted base"),
        b"same bytes",
    );
    store.insert(
        target_selection,
        target_segment.admitted_id().expect("admitted target"),
        b"same bytes",
    );
    store
        .delay_roots
        .insert(*base.root().as_bytes(), Duration::from_millis(4));
    let mut policy = limits();
    policy.warm_uses = u32::MAX;
    let mut cache = AdaptiveIrResidency::new(policy);

    let first = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            <[u8]>::len,
        )
        .expect("first route probes delta CAS")
        .0;
    let second = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            <[u8]>::len,
        )
        .expect("second route probes pristine CAS")
        .0;
    let third = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            <[u8]>::len,
        )
        .expect("measured cost chooses the faster pristine CAS")
        .0;

    assert!(matches!(first, IrResidencyPath::DeltaCas(_)));
    assert_eq!(
        second,
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaBaselineProbe)
    );
    assert_eq!(
        third,
        IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaCostHigher)
    );

    // A selected route is a measured choice, not a permanent verdict. After
    // thirty-two cold reads, sample the still-valid delta again. The storage
    // locality has changed: the historical CAS is now warm while pristine
    // target reads have become expensive.
    for _ in 0..29 {
        let route = cache
            .with_segment(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &chain,
                &mut store,
                <[u8]>::len,
            )
            .expect("pristine remains cheaper before scheduled resampling")
            .0;
        assert_eq!(
            route,
            IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaCostHigher)
        );
    }
    store.delay_roots.remove(base.root().as_bytes());
    store
        .delay_roots
        .insert(*target.root().as_bytes(), Duration::from_millis(4));
    let reprobe = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            <[u8]>::len,
        )
        .expect("periodic delta reprobe")
        .0;
    assert!(matches!(reprobe, IrResidencyPath::DeltaCas(_)));
    let mut relearned = false;
    for _ in 0..8 {
        let route = cache
            .with_segment(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &chain,
                &mut store,
                <[u8]>::len,
            )
            .expect("selected route after locality change")
            .0;
        relearned |= matches!(route, IrResidencyPath::DeltaCas(_));
    }
    assert!(relearned, "the cache must relearn a cheaper delta route");
}

#[test]
fn measured_fast_delta_route_remains_preferred_after_the_baseline_probe() {
    let base = manifest(1, b"same bytes");
    let target = manifest(2, b"same bytes");
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segment = &base.plane(base_selection.kind()).expect("plane").segments()[0];
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let chain = [IrResidencyDeltaHop::new(
        &base,
        &target,
        base_selection,
        base.root(),
    )];
    let mut store = MemoryRangeStore::default();
    store.insert(
        base_selection,
        base_segment.admitted_id().expect("admitted base"),
        b"same bytes",
    );
    store.insert(
        target_selection,
        target_segment.admitted_id().expect("admitted target"),
        b"same bytes",
    );
    store
        .delay_roots
        .insert(*target.root().as_bytes(), Duration::from_millis(4));
    let mut policy = limits();
    policy.warm_uses = u32::MAX;
    let mut cache = AdaptiveIrResidency::new(policy);
    for _ in 0..2 {
        cache
            .with_segment(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &chain,
                &mut store,
                <[u8]>::len,
            )
            .expect("collect delta and pristine costs");
    }
    let chosen = cache
        .with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &chain,
            &mut store,
            <[u8]>::len,
        )
        .expect("measured cost chooses faster delta CAS")
        .0;
    assert!(matches!(chosen, IrResidencyPath::DeltaCas(_)));
}

#[test]
fn retained_leases_can_lend_concurrent_borrows_with_fresh_owner_checks() {
    let manifest = manifest(1, b"parallel semantic bytes");
    let (mut source, selection) = selection_for(&manifest, 1);
    let segment = &manifest.plane(selection.kind()).expect("plane").segments()[0];
    let mut store = MemoryRangeStore::default();
    store.insert(
        selection,
        segment.admitted_id().expect("admitted"),
        b"parallel semantic bytes",
    );
    let mut cache = AdaptiveIrResidency::new(IrResidencyLimits {
        warm_uses: 1,
        ..limits()
    });
    cache
        .with_segment(
            &mut source,
            selection,
            &manifest,
            segment,
            &[],
            &mut store,
            |_| (),
        )
        .expect("segment becomes hot");
    let lease = cache
        .lease_hot_segment(&mut source, selection, &manifest, segment)
        .expect("lease lookup")
        .expect("hot lease");
    assert_eq!(
        cache.metrics().pinned_owner_bytes,
        b"parallel semantic bytes".len() as u64
    );

    let (left, right) = std::thread::scope(|scope| {
        let first_lease = lease.clone();
        let second_lease = lease.clone();
        let mut first_source = source.clone();
        let mut second_source = source.clone();
        let first = scope.spawn(move || {
            first_lease
                .with_payload(&mut first_source, <[u8]>::len)
                .expect("first current owner check")
        });
        let second = scope.spawn(move || {
            second_lease
                .with_payload(&mut second_source, <[u8]>::len)
                .expect("second current owner check")
        });
        (
            first.join().expect("first reader"),
            second.join().expect("second reader"),
        )
    });
    assert_eq!(left, b"parallel semantic bytes".len());
    assert_eq!(right, left);
    assert_eq!(cache.metrics().pinned_owner_bytes, left as u64);
    drop(lease);
    assert_eq!(cache.metrics().pinned_owner_bytes, 0);
}

#[test]
#[ignore = "manual repeatable microbenchmark; run in release mode with --ignored --nocapture"]
fn benchmark_cold_hot_and_delta_segment_paths() {
    const ITERATIONS: usize = 256;
    const PAYLOAD_BYTES: usize = 64 * 1024;
    let payload = vec![0x5a; PAYLOAD_BYTES];
    let base = manifest(1, &payload);
    let target = manifest(2, &payload);
    let (_, base_selection) = selection_for(&base, 1);
    let (mut target_source, target_selection) = selection_for(&target, 2);
    let base_segment = &base.plane(base_selection.kind()).expect("plane").segments()[0];
    let target_segment = &target
        .plane(target_selection.kind())
        .expect("plane")
        .segments()[0];
    let hop = IrResidencyDeltaHop::new(&base, &target, base_selection, base.root());
    let chain = [hop];
    let mut store = MemoryRangeStore::default();
    store.insert(
        base_selection,
        base_segment.admitted_id().expect("base admitted"),
        &payload,
    );
    store.insert(
        target_selection,
        target_segment.admitted_id().expect("target admitted"),
        &payload,
    );

    let mut cold_limits = limits();
    cold_limits.warm_uses = u32::MAX;
    let mut cold = AdaptiveIrResidency::new(cold_limits);
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        cold.with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &[],
            &mut store,
            <[u8]>::len,
        )
        .expect("cold exact segment read");
    }
    let cold_elapsed = started.elapsed();

    let mut hot_limits = limits();
    hot_limits.hot_bytes = PAYLOAD_BYTES as u64;
    hot_limits.pinned_bytes = PAYLOAD_BYTES as u64;
    let mut hot = AdaptiveIrResidency::new(hot_limits);
    for _ in 0..2 {
        hot.with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &[],
            &mut store,
            <[u8]>::len,
        )
        .expect("warm exact segment read");
    }
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        hot.with_segment(
            &mut target_source,
            target_selection,
            &target,
            target_segment,
            &[],
            &mut store,
            <[u8]>::len,
        )
        .expect("hot borrowed segment read");
    }
    let hot_elapsed = started.elapsed();
    assert_eq!(hot.metrics().hot_hits, ITERATIONS as u64);
    assert_eq!(hot.metrics().hot_admissions, 1);

    let mut delta_limits = limits();
    delta_limits.warm_uses = u32::MAX;
    let mut delta = AdaptiveIrResidency::new(delta_limits);
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        delta
            .with_segment(
                &mut target_source,
                target_selection,
                &target,
                target_segment,
                &chain,
                &mut store,
                <[u8]>::len,
            )
            .expect("bounded delta segment read");
    }
    let delta_elapsed = started.elapsed();
    assert!(delta.metrics().delta_reads > 0);
    let ns_per_op = |duration: Duration| duration.as_nanos() / ITERATIONS as u128;
    println!(
        "segment_bytes={PAYLOAD_BYTES} iterations={ITERATIONS} cold_cas_ns/op={} hot_borrow_ns/op={} adaptive_delta_route_ns/op={} cold_reads={} hot_hits={} delta_reads={} delta_plan_ns={} delta_actions={} delta_changed_bytes={}",
        ns_per_op(cold_elapsed),
        ns_per_op(hot_elapsed),
        ns_per_op(delta_elapsed),
        cold.metrics().cold_reads,
        hot.metrics().hot_hits,
        delta.metrics().delta_reads,
        delta.metrics().delta_plan_ns,
        delta.metrics().delta_actions_scanned,
        delta.metrics().delta_changed_bytes_scanned,
    );
}
