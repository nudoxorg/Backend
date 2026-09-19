//! Physical trace checkpoint descriptors and durable CAS hydration.
//!
//! Checkpoints have two storage layers.  The small typed descriptor is kept in
//! a store closure and contains only roots, frontiers, limits, and immutable
//! object references.  Visible state and history payloads are separate CAS
//! objects: visible state is a path-copied tree of flow node objects and each
//! retained delta is a `RunSchema` object.  This keeps a one-row checkpoint
//! update local to the changed tree path and the bounded history tail.

use super::{Arrangement, BatchRecord, DurableCache};
#[cfg(test)]
use super::{WorkCounters, WorkScope};
#[cfg(test)]
use crate::TraceSpine;
use crate::{
    ArrangementRoot, CanonicalValue, Delta, FlowError, Frontier, LayoutId, RowKey, Run, RunRoot,
    Time,
};
use backend_store::{
    ClosureId, ClosureManifest, FileStore, StoreError, TypedObject, UntrustedObjectId,
};
#[cfg(test)]
use backend_version::ObjectVersion;
use backend_version::{ObjectKey, Schema, TreeNodeView};
#[cfg(test)]
use std::{collections::HashSet, mem::size_of, sync::Mutex};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    marker::PhantomData,
    sync::Arc,
};

const CHECKPOINT_MAGIC: &[u8] = b"flow.trace.checkpoint.v2\0";
const NODE_MAGIC: &[u8] = b"flow.trace.node.v1\0";
const CHECKPOINT_SCHEMA_TYPE: u16 = 11;
const NODE_SCHEMA_TYPE: u16 = 12;
const NODE_EMPTY: u8 = 0;
const NODE_LEAF: u8 = 1;
const NODE_BRANCH: u8 = 2;
const MAX_WIRE_COUNT: usize = 1_000_000;
const MAX_REFERENCE_INDEX_DEPTH: u16 = 64;

/// Schema for immutable checkpoint descriptors stored in the object CAS.
pub struct CheckpointSchema;
impl Schema for CheckpointSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = CHECKPOINT_SCHEMA_TYPE;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema for one immutable visible-state tree node.
///
/// The value is a checked flow node encoding.  Branches contain physical CAS
/// object references for their children, while leaves contain typed row keys,
/// values, and supports.  The schema is intentionally an ordinary object
/// schema; hydration authenticates the transitive tree and then recomputes the
/// typed arrangement root.
pub(crate) struct NodeSchema;
impl Schema for NodeSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = NODE_SCHEMA_TYPE;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Decodes a value from the canonical value lane of a persisted trace row.
pub trait CheckpointValue: Clone + Ord + Debug + Eq + CanonicalValue + 'static {
    /// Decodes one value and advances the supplied input slice.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidRoot`] when the value tag or bytes are
    /// malformed, or [`FlowError::Overflow`] when a checked length cannot be
    /// represented.
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError>;

    /// Decodes the ordering representation used when this value is part of a
    /// persistent relation key. It defaults to the payload representation.
    ///
    /// # Errors
    ///
    /// Propagates the value decoder's malformed or overflow error.
    fn decode_checkpoint_ordered(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        Self::decode_checkpoint(bytes)
    }
}

/// Resolves opaque row identity bytes during checkpoint hydration.
pub trait CheckpointKeyDecoder {
    /// Resolves one persisted row identity.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidRoot`] when the identity is unknown or does
    /// not resolve to the exact supplied relation/object/key tuple.
    fn decode_key(
        &self,
        relation: &[u8; 32],
        object: &[u8; 32],
        key: u64,
    ) -> Result<RowKey, FlowError>;
}

impl<F> CheckpointKeyDecoder for F
where
    F: Fn(&[u8; 32], &[u8; 32], u64) -> Result<RowKey, FlowError>,
{
    fn decode_key(
        &self,
        relation: &[u8; 32],
        object: &[u8; 32],
        key: u64,
    ) -> Result<RowKey, FlowError> {
        self(relation, object, key)
    }
}

macro_rules! fixed_checkpoint_value {
    ($ty:ty, $tag:expr, $width:expr, $ordered:expr) => {
        impl CheckpointValue for $ty {
            fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
                if bytes.first().copied() != Some($tag) {
                    return Err(FlowError::InvalidRoot);
                }
                let _ = take_bytes(bytes, 1)?;
                let encoded = take_bytes(bytes, $width)?;
                let array: [u8; $width] = encoded.try_into().map_err(|_| FlowError::InvalidRoot)?;
                Ok(<$ty>::from_be_bytes(array))
            }
            fn decode_checkpoint_ordered(bytes: &mut &[u8]) -> Result<Self, FlowError> {
                if bytes.first().copied() != Some($tag) {
                    return Err(FlowError::InvalidRoot);
                }
                let _ = take_bytes(bytes, 1)?;
                let encoded = take_bytes(bytes, $width)?;
                let array: [u8; $width] = encoded.try_into().map_err(|_| FlowError::InvalidRoot)?;
                Ok(($ordered)(<$ty>::from_be_bytes(array)))
            }
        }
    };
}

fixed_checkpoint_value!(i64, b'i', 8, |value: i64| {
    i64::from_be_bytes((value.cast_unsigned() ^ (1 << 63)).to_be_bytes())
});
fixed_checkpoint_value!(u64, b'U', 8, |value: u64| value);
fixed_checkpoint_value!(i32, b'j', 4, |value: i32| {
    i32::from_be_bytes((value.cast_unsigned() ^ (1 << 31)).to_be_bytes())
});
fixed_checkpoint_value!(u32, b'J', 4, |value: u32| value);
fixed_checkpoint_value!(i16, b'h', 2, |value: i16| {
    i16::from_be_bytes((value.cast_unsigned() ^ (1 << 15)).to_be_bytes())
});
fixed_checkpoint_value!(u16, b'H', 2, |value: u16| value);

impl CheckpointValue for i8 {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'g') {
            return Err(FlowError::InvalidRoot);
        }
        let _ = take_bytes(bytes, 1)?;
        Ok(i8::from_be_bytes([take_bytes(bytes, 1)?[0]]))
    }
    fn decode_checkpoint_ordered(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'g') {
            return Err(FlowError::InvalidRoot);
        }
        let _ = take_bytes(bytes, 1)?;
        Ok(i8::from_be_bytes([take_bytes(bytes, 1)?[0] ^ 0x80]))
    }
}

impl CheckpointValue for u8 {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'G') {
            return Err(FlowError::InvalidRoot);
        }
        let _ = take_bytes(bytes, 1)?;
        Ok(take_bytes(bytes, 1)?[0])
    }
}

impl CheckpointValue for usize {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'Z') {
            return Err(FlowError::InvalidRoot);
        }
        usize::try_from(read_u64_after_tag(bytes)?).map_err(|_| FlowError::Overflow)
    }
}

impl CheckpointValue for bool {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'B') {
            return Err(FlowError::InvalidRoot);
        }
        match take_bytes(bytes, 2)?[1] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(FlowError::InvalidRoot),
        }
    }
}

impl CheckpointValue for String {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'S') {
            return Err(FlowError::InvalidRoot);
        }
        let length = checked_length_after_tag(bytes)?;
        String::from_utf8(take_bytes(bytes, length)?.to_vec()).map_err(|_| FlowError::InvalidRoot)
    }
    fn decode_checkpoint_ordered(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        decode_ordered_bytes(bytes, b'S')
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| FlowError::InvalidRoot))
    }
}

impl CheckpointValue for Vec<u8> {
    fn decode_checkpoint(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        if bytes.first().copied() != Some(b'V') {
            return Err(FlowError::InvalidRoot);
        }
        let length = checked_length_after_tag(bytes)?;
        Ok(take_bytes(bytes, length)?.to_vec())
    }
    fn decode_checkpoint_ordered(bytes: &mut &[u8]) -> Result<Self, FlowError> {
        decode_ordered_bytes(bytes, b'V')
    }
}

fn decode_ordered_bytes(bytes: &mut &[u8], tag: u8) -> Result<Vec<u8>, FlowError> {
    if bytes.first().copied() != Some(tag) {
        return Err(FlowError::InvalidRoot);
    }
    let _ = take_bytes(bytes, 1)?;
    let mut output = Vec::new();
    loop {
        let byte = take_bytes(bytes, 1)?[0];
        if byte != 0 {
            output.push(byte);
            continue;
        }
        match take_bytes(bytes, 1)?[0] {
            0 => return Ok(output),
            0xff => output.push(0),
            _ => return Err(FlowError::InvalidRoot),
        }
    }
}

/// A physical tree node reference stored inside a checkpoint descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObjectRef {
    logical: [u8; 32],
    object: [u8; 32],
}

#[derive(Clone, Debug)]
struct NodeChild<V> {
    logical: [u8; 32],
    object: [u8; 32],
    first: crate::ArrangementKey<V>,
    row_count: u64,
}

#[derive(Debug)]
struct DecodedNode<V> {
    logical: [u8; 32],
    level: u16,
    entries: Option<Vec<(crate::ArrangementKey<V>, i64)>>,
    children: Vec<NodeChild<V>>,
}

/// A physical trace checkpoint. Logical root identity is independent of its
/// layout and segment grouping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceCheckpoint<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    /// Physical layout identity.
    pub layout: LayoutId,
    /// Immutable consolidated segment digests.
    pub base_segments: Vec<RunRoot>,
    /// Bounded delta run digests.
    pub delta_runs: Vec<RunRoot>,
    /// Upper frontier at checkpoint creation.
    pub upper: Frontier,
    /// Since frontier at checkpoint creation.
    pub since: Time,
    /// Complete retained since antichain at checkpoint creation.
    pub since_frontier: Frontier,
    /// Logical arrangement root.
    pub logical_root: ArrangementRoot<V>,
    /// Rows represented by the checkpoint.
    pub retained_rows: u64,
    /// Payload marker retained for typed APIs.
    pub(crate) _marker: PhantomData<fn() -> V>,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> TraceCheckpoint<V> {
    /// Returns the number of immutable physical segments.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.base_segments.len() + self.delta_runs.len()
    }
}

/// A trace checkpoint whose immutable contents have been admitted to the
/// filesystem object CAS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableTraceCheckpoint<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    /// Checked logical/physical checkpoint descriptor.
    pub checkpoint: TraceCheckpoint<V>,
    /// Immutable closure containing the descriptor object.
    pub closure: ClosureId,
    /// Complete physical level layout used to hydrate the arrangement.
    pub level_runs: Vec<Vec<RunRoot>>,
    /// Physical object identity of the visible tree root.
    pub visible_root_object: [u8; 32],
    /// Sequence of the newest retained transition.
    pub next_batch: u64,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> DurableTraceCheckpoint<V> {
    /// Returns the immutable closure identity used by this checkpoint.
    #[must_use]
    pub const fn closure_id(&self) -> ClosureId {
        self.closure
    }

    /// Returns the physical run count across all levels.
    #[must_use]
    pub fn run_count(&self) -> usize {
        self.level_runs.iter().map(Vec::len).sum()
    }

    /// Opens the authenticated visible root as a lazy canonical relation.
    ///
    /// This is the low-level reopen seam for callers that only need root
    /// metadata, point probes, or path-copy updates.  It reads and admits the
    /// root relation node through the store's typed relation index and leaves
    /// runs, history payloads, and all unrelated descendants untouched.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the checkpoint root is not present
    /// in the store's registered canonical relation index or fails admission.
    pub fn open_lazy_root(
        &self,
        store: &FileStore,
    ) -> Result<OwnedLazyCheckpointRoot<crate::ArrangementCanonicalRelation<V>>, StoreError>
    where
        V: CheckpointValue,
    {
        let claim = arrangement_root_claim(self.checkpoint.logical_root)
            .map_err(|_| StoreError::Corrupt)?;
        OwnedLazyCheckpointRoot::open_flow(store, claim, self.visible_root_object)
    }

    /// Hydrates an arrangement from this checkpoint using an unlimited work
    /// scope.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the closure, manifest, or any
    /// referenced immutable object fails checked admission, and
    /// [`StoreError::Io`] for storage failures.
    #[cfg(test)]
    pub fn hydrate<K: CheckpointKeyDecoder>(
        &self,
        store: &FileStore,
        key_decoder: &K,
    ) -> Result<Arrangement<V>, StoreError>
    where
        V: CheckpointValue,
    {
        let mut scope = WorkScope::new(usize::MAX, usize::MAX);
        self.hydrate_budgeted(store, key_decoder, &mut scope)
    }

    /// Hydrates an arrangement under a checked row/byte/probe envelope.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NeedsScopedRebuild`] when the supplied work
    /// scope is exhausted, [`StoreError::Corrupt`] for invalid checkpoint
    /// contents, and [`StoreError::Io`] for storage failures.
    #[cfg(test)]
    pub fn hydrate_budgeted<K: CheckpointKeyDecoder>(
        &self,
        store: &FileStore,
        key_decoder: &K,
        scope: &mut WorkScope,
    ) -> Result<Arrangement<V>, StoreError>
    where
        V: CheckpointValue,
    {
        hydrate::hydrate_checkpoint(self, store, key_decoder, scope)
    }
}

/// Measured durable bytes admitted while writing one checkpoint closure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckpointWriteReport {
    /// Number of newly created immutable objects and manifest bytes.
    pub bytes_written: u64,
    /// Number of newly created immutable objects.
    pub objects_written: u64,
    /// Physical rows encoded into newly created run objects.
    pub rows_written: u64,
}

fn store_error(error: FlowError) -> StoreError {
    match error {
        FlowError::Overflow => StoreError::Bounds,
        FlowError::RecursionWorkLimit => StoreError::NeedsScopedRebuild,
        _ => StoreError::Corrupt,
    }
}

fn object_exists(store: &FileStore, id: &[u8; 32]) -> Result<bool, StoreError> {
    match store.read_object_claim(UntrustedObjectId::from_bytes(*id)) {
        Ok(_) => Ok(true),
        Err(StoreError::Corrupt) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn read_schema_object<S: Schema<Value = Vec<u8>>>(
    store: &FileStore,
    id: &[u8; 32],
) -> Result<Option<Vec<u8>>, StoreError> {
    let object = match store.read_object_claim(UntrustedObjectId::from_bytes(*id)) {
        Ok(object) => object,
        Err(StoreError::Corrupt) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !schema_matches::<S>(&object) {
        return Err(StoreError::Corrupt);
    }
    let payload = object.bytes().to_vec();
    let typed = TypedObject::from_value(&ObjectKey::<S>::from_value(&payload), &payload);
    if typed.id() != object.id()
        || typed.key() != object.key()
        || typed.version() != object.version()
    {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(payload))
}

#[derive(Clone, Debug)]
struct HistoryRef {
    sequence: u64,
    before: [u8; 32],
    after: [u8; 32],
    bytes: usize,
    run: ObjectRef,
}

type PersistedLevels = (Vec<Vec<ObjectRef>>, BTreeMap<[u8; 32], ObjectRef>);

#[derive(Clone, Copy)]
struct ReferenceIndexInput<'a, V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    arrangement: &'a Arrangement<V>,
    levels: &'a [Vec<ObjectRef>],
    history: &'a [HistoryRef],
    new_objects: &'a BTreeSet<[u8; 32]>,
}

#[cfg(test)]
struct HydrationState {
    cache: DurableCache,
    retained_objects: BTreeSet<[u8; 32]>,
}

#[derive(Debug)]
#[cfg(test)]
struct DecodedHistory {
    sequence: u64,
    before: [u8; 32],
    after: [u8; 32],
    bytes: usize,
    run: ObjectRef,
}

#[derive(Debug)]
#[cfg(test)]
struct DecodedManifest {
    layout: [u8; 32],
    logical_root: [u8; 32],
    next_batch: u64,
    retained_rows: u64,
    since: Time,
    upper: Frontier,
    since_frontier: Frontier,
    visible: ObjectRef,
    reference_index: ObjectRef,
    reference_index_depth: u16,
    levels: Vec<Vec<ObjectRef>>,
    history: Vec<DecodedHistory>,
    max_history: usize,
    max_history_rows: usize,
    max_history_bytes: usize,
    max_subscription_events: usize,
    max_subscription_rows: usize,
}

#[derive(Debug)]
#[cfg(test)]
struct LoadedNode<V> {
    logical: [u8; 32],
    first: Option<crate::ArrangementKey<V>>,
    last: Option<crate::ArrangementKey<V>>,
    /// Sorted leaf entries carried only through hydration. The arrangement
    /// itself retains them in the typed persistent relation state.
    visible: Vec<(crate::ArrangementKey<V>, i64)>,
}

#[cfg(test)]
type HydratedLevels<V> = (Vec<Vec<Arc<Run<V>>>>, u64);
#[cfg(test)]
type HydratedHistory<V> = Vec<(DecodedHistory, Vec<Delta<V>>)>;

#[cfg(test)]
fn schema_matches<S: Schema>(object: &TypedObject) -> bool {
    let schema = object.schema();
    schema.domain() == S::DOMAIN && schema.ty() == S::TYPE && schema.version() == S::VERSION
}

mod codec;
#[cfg(test)]
mod hydrate;
mod lazy;
mod persist;
mod reference_index;

pub use lazy::{
    CasNodeLoader, CasNodeReader, FileStoreNodeReader, LazyCheckpointRoot, OwnedLazyCheckpointRoot,
    arrangement_root_claim,
};

use codec::{checked_length_after_tag, read_u64_after_tag, take_bytes};

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    /// Writes immutable tree nodes, physical runs, and a bounded descriptor
    /// to the store CAS. Existing node/run objects are reused by root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a canonical node or immutable
    /// object fails admission, [`StoreError::Bounds`] when checked wire or
    /// accounting limits overflow, and [`StoreError::Io`] for storage
    /// failures.
    pub fn write_checkpoint(
        &self,
        store: &FileStore,
        layout: LayoutId,
    ) -> Result<(DurableTraceCheckpoint<V>, CheckpointWriteReport), StoreError> {
        let checkpoint = self.checkpoint(layout);
        let mut report = CheckpointWriteReport::default();
        let mut cache = self.durable_cache.lock().map_err(|_| StoreError::Corrupt)?;
        persist::prune_history_cache(self, &mut cache);
        let mut new_objects = BTreeSet::new();

        let mut closure = self.state.node_closure();
        let root: TreeNodeView<'_, crate::ArrangementRelation<V>> =
            closure.next().ok_or(StoreError::Corrupt)?;
        let visible =
            persist::persist_node(root, store, &mut cache, &mut report, &mut new_objects)?;
        if visible.logical != *self.state.root().as_bytes() {
            return Err(StoreError::Corrupt);
        }
        let (levels, run_refs) = persist::persist_levels(
            &self.levels,
            store,
            &mut cache,
            &mut report,
            &mut new_objects,
        )?;
        let history = persist::persist_history(
            &self.history,
            &run_refs,
            store,
            &mut cache,
            &mut report,
            &mut new_objects,
        )?;
        let (reference_index_object, reference_index, reference_index_depth) =
            reference_index::select_reference_index(
                &ReferenceIndexInput {
                    arrangement: self,
                    levels: &levels,
                    history: &history,
                    new_objects: &new_objects,
                },
                store,
                &mut cache,
                &mut report,
            )?;
        cache.latest_index = Some((
            *self.state.root().as_bytes(),
            self.next_batch,
            reference_index.object,
            reference_index_depth,
        ));

        let manifest_bytes = codec::encode_manifest(
            self,
            &checkpoint,
            visible,
            reference_index,
            reference_index_depth,
            &levels,
            &history,
        )
        .map_err(store_error)?;
        let manifest_object = TypedObject::from_value(
            &ObjectKey::<CheckpointSchema>::from_value(&manifest_bytes),
            &manifest_bytes,
        );
        persist::persist_object(store, &manifest_object, &mut report)?;
        let closure = ClosureManifest::new_with_registry(
            vec![manifest_object, reference_index_object],
            store.relation_registry(),
        )
        .map_err(|_| StoreError::Corrupt)?;
        let closure_id = store.write_closure(&closure)?;
        Ok((
            DurableTraceCheckpoint {
                checkpoint,
                closure: closure_id,
                level_runs: self
                    .levels
                    .iter()
                    .map(|level| level.iter().map(|run| run.root()).collect())
                    .collect(),
                visible_root_object: visible.object,
                next_batch: self.next_batch,
            },
            report,
        ))
    }

    /// Alias for [`Arrangement::write_checkpoint`] at the arrangement seam.
    ///
    /// # Errors
    ///
    /// Propagates the checked storage and canonicalization errors from
    /// [`Arrangement::write_checkpoint`].
    pub fn checkpoint_to_store(
        &self,
        store: &FileStore,
        layout: LayoutId,
    ) -> Result<(DurableTraceCheckpoint<V>, CheckpointWriteReport), StoreError> {
        self.write_checkpoint(store, layout)
    }

    /// Reopens a durable checkpoint with an unlimited foreground scope.
    ///
    /// # Errors
    ///
    /// Propagates the checked hydration and storage errors from
    /// [`DurableTraceCheckpoint::hydrate`].
    #[cfg(test)]
    pub fn from_checkpoint<K: CheckpointKeyDecoder>(
        store: &FileStore,
        checkpoint: &DurableTraceCheckpoint<V>,
        key_decoder: &K,
    ) -> Result<Self, StoreError>
    where
        V: CheckpointValue,
    {
        checkpoint.hydrate(store, key_decoder)
    }

    /// Reopens a durable checkpoint under a checked row/byte/probe envelope.
    ///
    /// # Errors
    ///
    /// Propagates the checked hydration, scope, and storage errors from
    /// [`DurableTraceCheckpoint::hydrate_budgeted`].
    #[cfg(test)]
    pub fn from_checkpoint_budgeted<K: CheckpointKeyDecoder>(
        store: &FileStore,
        checkpoint: &DurableTraceCheckpoint<V>,
        key_decoder: &K,
        scope: &mut WorkScope,
    ) -> Result<Self, StoreError>
    where
        V: CheckpointValue,
    {
        checkpoint.hydrate_budgeted(store, key_decoder, scope)
    }
}
