//! Checked checkpoint hydration, frontier binding, and history replay.

use super::{
    Arc, Arrangement, BTreeMap, BTreeSet, BatchRecord, CanonicalValue, CheckpointKeyDecoder,
    CheckpointSchema, CheckpointValue, Debug, DecodedHistory, DecodedManifest, DecodedNode, Delta,
    DurableCache, DurableTraceCheckpoint, FileStore, FlowError, HashSet, HydratedHistory,
    HydratedLevels, HydrationState, LoadedNode, MAX_WIRE_COUNT, Mutex, NodeSchema, ObjectRef,
    ObjectVersion, RowKey, Run, StoreError, TraceCheckpoint, TraceSpine, WorkCounters, WorkScope,
    read_schema_object, schema_matches, size_of, store_error,
};

use super::{codec, reference_index};
use crate::ArrangementKey;
use crate::batch::{canonical_bytes, consolidate_rows};
use backend_version::{DeltaError, MapChange, RelationState, prepare_delta_with_state};
use codec::{
    checked_count, decode_manifest, read_count, read_length_prefixed_root, read_root, read_time,
    read_u64, take_bytes,
};

fn map_delta_error(error: DeltaError) -> StoreError {
    match error {
        DeltaError::IncompleteBase
        | DeltaError::BaseMismatch
        | DeltaError::BeforeMismatch
        | DeltaError::Unsorted
        | DeltaError::DuplicateKey
        | DeltaError::TargetMismatch
        | DeltaError::NonAdjacent
        | DeltaError::CompositionMismatch
        | DeltaError::Canonical(_) => StoreError::Corrupt,
    }
}

fn run_payload<V: CheckpointValue, K: CheckpointKeyDecoder>(
    store: &FileStore,
    reference: ObjectRef,
    key_decoder: &K,
    scope: &mut WorkScope,
) -> Result<Vec<Delta<V>>, StoreError> {
    let payload = read_schema_object::<crate::RunSchema>(store, &reference.object)?
        .ok_or(StoreError::Corrupt)?;
    if ObjectVersion::<crate::RunSchema>::from_value(&payload).as_bytes() != &reference.logical {
        return Err(StoreError::Corrupt);
    }
    decode_run(&payload, key_decoder, scope).map_err(store_error)
}

pub(super) fn decode_run<V: CheckpointValue, K: CheckpointKeyDecoder>(
    mut bytes: &[u8],
    key_decoder: &K,
    scope: &mut WorkScope,
) -> Result<Vec<Delta<V>>, FlowError> {
    let encoded = bytes;
    let header = b"flow.batch.v2\0";
    if !encoded.starts_with(header) {
        return Err(FlowError::InvalidRoot);
    }
    scope.charge_bytes(encoded.len())?;
    bytes = &encoded[header.len()..];
    if read_root(&mut bytes)? != [0; 32] {
        return Err(FlowError::InvalidRoot);
    }
    let count = checked_count(bytes, read_count(&mut bytes)?, 32 + 32 + 8 + 10 + 8 + 1)?;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let relation = read_length_prefixed_root(&mut bytes)?;
        let object = read_length_prefixed_root(&mut bytes)?;
        let key = read_u64(&mut bytes)?;
        let time = read_time(&mut bytes)?;
        let diff = i64::from_be_bytes(
            take_bytes(&mut bytes, 8)?
                .try_into()
                .map_err(|_| FlowError::InvalidRoot)?,
        );
        let row_key = key_decoder.decode_key(&relation, &object, key)?;
        if row_key.relation.as_bytes() != &relation
            || row_key.object.as_bytes() != &object
            || row_key.key != key
        {
            return Err(FlowError::InvalidRoot);
        }
        let value = V::decode_checkpoint(&mut bytes)?;
        let value_bytes = size_of::<Delta<V>>()
            .checked_add(value.owned_bytes())
            .ok_or(FlowError::Overflow)?;
        scope.charge_row_bytes(1, value_bytes)?;
        rows.push(Delta::checked(row_key, value, time, diff)?);
    }
    if !bytes.is_empty() || canonical_bytes(&rows, [0; 32]) != encoded {
        return Err(FlowError::InvalidRoot);
    }
    consolidate_rows(rows)
}

fn hydrate_node<V: CheckpointValue, K: CheckpointKeyDecoder>(
    store: &FileStore,
    reference: ObjectRef,
    expected_level: Option<u16>,
    key_decoder: &K,
    scope: &mut WorkScope,
    visited: &mut HashSet<[u8; 32]>,
    cache: &mut DurableCache,
) -> Result<LoadedNode<V>, StoreError> {
    if !visited.insert(reference.object) {
        return Err(StoreError::Corrupt);
    }
    if visited.len() > MAX_WIRE_COUNT {
        return Err(StoreError::Bounds);
    }
    let payload =
        read_schema_object::<NodeSchema>(store, &reference.object)?.ok_or(StoreError::Corrupt)?;
    scope.charge_bytes(payload.len()).map_err(store_error)?;
    let node: DecodedNode<V> = codec::decode_node(&payload, key_decoder).map_err(store_error)?;
    if node.logical != reference.logical || expected_level.is_some_and(|level| level != node.level)
    {
        return Err(StoreError::Corrupt);
    }
    cache.nodes.insert(node.logical, reference.object);

    if let Some(entries) = node.entries {
        let mut visible = Vec::with_capacity(entries.len());
        let mut previous: Option<ArrangementKey<V>> = None;
        for (key, support) in entries {
            let (key, value) = key.into_parts();
            let bytes = size_of::<(RowKey, V)>()
                .checked_add(value.owned_bytes())
                .ok_or(StoreError::Bounds)?;
            scope.charge_row_bytes(1, bytes).map_err(store_error)?;
            let current = ArrangementKey::new(key, value.clone());
            if previous
                .as_ref()
                .is_some_and(|previous| previous >= &current)
            {
                return Err(StoreError::Corrupt);
            }
            visible.push((ArrangementKey::new(key, value), support));
            previous = Some(current);
        }
        let first = visible.first().map(|(key, _)| key.clone());
        let last = visible.last().map(|(key, _)| key.clone());
        return Ok(LoadedNode {
            logical: node.logical,
            first,
            last,
            visible,
        });
    }

    let child_level = node.level.checked_sub(1).ok_or(StoreError::Corrupt)?;
    let mut visible = Vec::new();
    let mut previous_last: Option<ArrangementKey<V>> = None;
    for child in node.children {
        let loaded = hydrate_node(
            store,
            ObjectRef {
                logical: child.logical,
                object: child.object,
            },
            Some(child_level),
            key_decoder,
            scope,
            visited,
            cache,
        )?;
        if loaded.first.as_ref() != Some(&child.first)
            || previous_last.as_ref().is_some_and(|previous| {
                loaded.first.as_ref().is_some_and(|first| previous >= first)
            })
        {
            return Err(StoreError::Corrupt);
        }
        previous_last.clone_from(&loaded.last);
        visible.extend(loaded.visible);
    }
    let first = visible.first().map(|(key, _)| key.clone());
    let last = visible.last().map(|(key, _)| key.clone());
    Ok(LoadedNode {
        logical: node.logical,
        first,
        last,
        visible,
    })
}

fn hydrate_levels<
    V: Clone + Ord + Debug + Eq + CanonicalValue + CheckpointValue + 'static,
    K: CheckpointKeyDecoder,
>(
    decoded: &DecodedManifest,
    durable: &DurableTraceCheckpoint<V>,
    trace: &TraceSpine,
    store: &FileStore,
    key_decoder: &K,
    scope: &mut WorkScope,
    state: &mut HydrationState,
) -> Result<HydratedLevels<V>, StoreError> {
    let mut levels = Vec::with_capacity(decoded.levels.len());
    let mut retained_rows = 0_u64;
    for (level_index, level) in decoded.levels.iter().enumerate() {
        let mut hydrated_level = Vec::with_capacity(level.len());
        for reference in level {
            state.retained_objects.insert(reference.object);
            let rows = run_payload(store, *reference, key_decoder, scope)?;
            retained_rows = retained_rows
                .checked_add(u64::try_from(rows.len()).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
            let run = Run::from_rows(rows, trace.clone()).map_err(store_error)?;
            if run.root().as_bytes() != &reference.logical {
                return Err(StoreError::Corrupt);
            }
            state.cache.runs.insert(reference.logical, reference.object);
            let expected = durable
                .level_runs
                .get(level_index)
                .and_then(|runs| runs.get(hydrated_level.len()))
                .ok_or(StoreError::Corrupt)?;
            if expected.as_bytes() != &reference.logical {
                return Err(StoreError::Corrupt);
            }
            hydrated_level.push(Arc::new(run));
        }
        levels.push(hydrated_level);
    }
    Ok((levels, retained_rows))
}

fn validate_level_layout<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    decoded: &DecodedManifest,
    checkpoint: &TraceCheckpoint<V>,
    durable: &DurableTraceCheckpoint<V>,
    retained_rows: u64,
) -> Result<(), StoreError> {
    if retained_rows != checkpoint.retained_rows || retained_rows != decoded.retained_rows {
        return Err(StoreError::Corrupt);
    }
    let expected_levels = durable
        .level_runs
        .iter()
        .map(|level| {
            level
                .iter()
                .map(|root| *root.as_bytes())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let decoded_levels = decoded
        .levels
        .iter()
        .map(|level| {
            level
                .iter()
                .map(|reference| reference.logical)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if expected_levels != decoded_levels {
        return Err(StoreError::Corrupt);
    }
    let expected_base = expected_levels
        .iter()
        .skip(1)
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let expected_delta = expected_levels.first().cloned().unwrap_or_default();
    if expected_base
        != checkpoint
            .base_segments
            .iter()
            .map(|root| *root.as_bytes())
            .collect::<Vec<_>>()
        || expected_delta
            != checkpoint
                .delta_runs
                .iter()
                .map(|root| *root.as_bytes())
                .collect::<Vec<_>>()
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn hydrate_history<V: CheckpointValue, K: CheckpointKeyDecoder>(
    records: Vec<DecodedHistory>,
    store: &FileStore,
    key_decoder: &K,
    scope: &mut WorkScope,
    cache: &mut DurableCache,
    retained_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<(HydratedHistory<V>, usize, usize), StoreError> {
    let mut decoded_history = Vec::with_capacity(records.len());
    let mut history_rows = 0_usize;
    let mut history_bytes = 0_usize;
    for record in records {
        retained_objects.insert(record.run.object);
        let deltas = run_payload(store, record.run, key_decoder, scope)?;
        let encoded = canonical_bytes(&deltas, [0; 32]);
        if record.bytes != encoded.len()
            || ObjectVersion::<crate::RunSchema>::from_value(&encoded).as_bytes()
                != &record.run.logical
        {
            return Err(StoreError::Corrupt);
        }
        cache
            .history
            .insert(record.sequence, (record.run.logical, record.run.object));
        history_rows = history_rows
            .checked_add(deltas.len())
            .ok_or(StoreError::Bounds)?;
        history_bytes = history_bytes
            .checked_add(record.bytes)
            .ok_or(StoreError::Bounds)?;
        decoded_history.push((record, deltas));
    }
    Ok((decoded_history, history_rows, history_bytes))
}

fn read_checkpoint_manifest<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    durable: &DurableTraceCheckpoint<V>,
    store: &FileStore,
    scope: &mut WorkScope,
) -> Result<DecodedManifest, StoreError> {
    // The closure is a persistent index. Open only its authenticated root and
    // fetch the bounded descriptor page; reading the flat closure would scan
    // every retained node/run before the checkpoint can be used.
    let manifest = store.open_closure(durable.closure)?;
    let page = manifest.page(None, 2)?;
    let metadata = page
        .objects()
        .iter()
        .find(|object| schema_matches::<CheckpointSchema>(object))
        .ok_or(StoreError::Corrupt)?;
    let metadata_bytes = metadata.bytes().to_vec();
    if ObjectVersion::<CheckpointSchema>::from_value(&metadata_bytes).as_bytes()
        != metadata.version()
    {
        return Err(StoreError::Corrupt);
    }
    let decoded = decode_manifest(&metadata_bytes, scope).map_err(store_error)?;
    Ok(decoded)
}

pub(super) fn validate_checkpoint_binding<
    V: Clone + Ord + Debug + Eq + CanonicalValue + 'static,
>(
    decoded: &DecodedManifest,
    durable: &DurableTraceCheckpoint<V>,
) -> Result<(), StoreError> {
    let checkpoint = &durable.checkpoint;
    if decoded.layout != *checkpoint.layout.as_bytes()
        || decoded.logical_root != *checkpoint.logical_root.as_bytes()
        || decoded.next_batch != durable.next_batch
        || decoded.upper != checkpoint.upper
        || decoded.since != checkpoint.since
        || decoded.since_frontier != checkpoint.since_frontier
        || decoded.retained_rows != checkpoint.retained_rows
        || decoded.levels.len() != durable.level_runs.len()
        || decoded.visible.object != durable.visible_root_object
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn validate_history_metadata<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    decoded: &DecodedManifest,
    history: &HydratedHistory<V>,
    history_rows: usize,
    history_bytes: usize,
) -> Result<(), StoreError> {
    if history.len() > decoded.max_history
        || history_rows > decoded.max_history_rows
        || history_bytes > decoded.max_history_bytes
    {
        return Err(StoreError::Corrupt);
    }
    for window in history.windows(2) {
        if window[0].0.sequence >= window[1].0.sequence {
            return Err(StoreError::Corrupt);
        }
    }
    if history
        .last()
        .is_some_and(|(record, _)| record.sequence > decoded.next_batch)
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn replay_history<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    state: &RelationState<crate::ArrangementRelation<V>>,
    decoded_history: HydratedHistory<V>,
    scope: &mut WorkScope,
) -> Result<Vec<BatchRecord<V>>, StoreError> {
    let mut current = state.clone();
    let mut history_roots = Vec::with_capacity(decoded_history.len());
    for (record, deltas) in decoded_history.iter().rev() {
        let after_root = current.root();
        if after_root.as_bytes() != &record.after {
            return Err(StoreError::Corrupt);
        }
        let mut pending: BTreeMap<ArrangementKey<V>, i64> = BTreeMap::new();
        for row in deltas {
            let key = ArrangementKey::new(row.key, row.value.clone());
            let old = pending
                .get(&key)
                .copied()
                .or_else(|| current.get(&key).copied())
                .unwrap_or_default();
            let next = old
                .checked_sub(row.diff.value())
                .ok_or(StoreError::Bounds)?;
            pending.insert(key, next);
        }
        let changes = pending
            .iter()
            .filter_map(|(key, next)| {
                let before = current.get(key).copied();
                let after = (*next != 0).then_some(*next);
                (before != after).then(|| MapChange {
                    key: key.clone(),
                    before,
                    after,
                })
            })
            .collect::<Vec<_>>();
        scope.charge_rows(changes.len()).map_err(store_error)?;
        let (prepared, _) = prepare_delta_with_state(&current, changes).map_err(map_delta_error)?;
        let before_root = prepared.delta().target();
        if before_root.as_bytes() != &record.before {
            return Err(StoreError::Corrupt);
        }
        let previous_state = prepared.commit(&current).map_err(map_delta_error)?;
        if previous_state.root() != before_root {
            return Err(StoreError::Corrupt);
        }
        history_roots.push((before_root, after_root));
        current = previous_state;
    }
    history_roots.reverse();
    Ok(decoded_history
        .into_iter()
        .zip(history_roots)
        .map(|((record, deltas), (before, after))| BatchRecord {
            sequence: record.sequence,
            before,
            after,
            deltas: Arc::from(deltas.into_boxed_slice()),
            bytes: record.bytes,
        })
        .collect())
}

pub(super) fn hydrate_checkpoint<
    V: Clone + Ord + Debug + Eq + CanonicalValue + CheckpointValue + 'static,
    K: CheckpointKeyDecoder,
>(
    durable: &DurableTraceCheckpoint<V>,
    store: &FileStore,
    key_decoder: &K,
    scope: &mut WorkScope,
) -> Result<Arrangement<V>, StoreError> {
    let mut decoded = read_checkpoint_manifest(durable, store, scope)?;
    validate_checkpoint_binding(&decoded, durable)?;
    let checkpoint = &durable.checkpoint;

    let mut cache = DurableCache::default();
    let mut visited = HashSet::new();
    let loaded: LoadedNode<V> = hydrate_node(
        store,
        decoded.visible,
        None,
        key_decoder,
        scope,
        &mut visited,
        &mut cache,
    )?;
    if loaded.logical != *checkpoint.logical_root.as_bytes() {
        return Err(StoreError::Corrupt);
    }
    let mut hydration = HydrationState {
        cache,
        retained_objects: visited.iter().copied().collect(),
    };
    let visible = loaded.visible;
    let visible_len = visible.len();
    let relation_state = RelationState::from_entries(visible, crate::arrangement_coverage())
        .map_err(|_| StoreError::Corrupt)?;
    if relation_state.root() != checkpoint.logical_root {
        return Err(StoreError::Corrupt);
    }

    let mut trace = TraceSpine::new();
    trace
        .advance_upper(decoded.upper.clone())
        .map_err(store_error)?;
    trace
        .advance_since_frontier(decoded.since_frontier.clone())
        .map_err(store_error)?;

    let (levels, retained_rows) = hydrate_levels(
        &decoded,
        durable,
        &trace,
        store,
        key_decoder,
        scope,
        &mut hydration,
    )?;
    validate_level_layout(&decoded, checkpoint, durable, retained_rows)?;

    hydration
        .retained_objects
        .extend(decoded.history.iter().map(|record| record.run.object));
    reference_index::validate_reference_index(&decoded, store, scope, &hydration.retained_objects)?;
    let history_records = std::mem::take(&mut decoded.history);
    let (decoded_history, history_rows, history_bytes) = hydrate_history(
        history_records,
        store,
        key_decoder,
        scope,
        &mut hydration.cache,
        &mut hydration.retained_objects,
    )?;
    hydration.cache.latest_index = Some((
        *checkpoint.logical_root.as_bytes(),
        decoded.next_batch,
        decoded.reference_index.object,
        decoded.reference_index_depth,
    ));
    validate_history_metadata(&decoded, &decoded_history, history_rows, history_bytes)?;

    let history = replay_history(&relation_state, decoded_history, scope)?;

    Ok(Arrangement {
        levels,
        trace,
        next_batch: decoded.next_batch,
        work: WorkCounters::default(),
        state: relation_state,
        visible_len,
        pins: super::super::PinRegistry::new(),
        history,
        history_rows,
        history_bytes,
        max_history: decoded.max_history,
        max_history_rows: decoded.max_history_rows,
        max_history_bytes: decoded.max_history_bytes,
        max_subscription_events: decoded.max_subscription_events,
        max_subscription_rows: decoded.max_subscription_rows,
        max_runs_per_level: 4,
        max_retained_runs: 64,
        max_retained_bytes: 64 * 1024 * 1024,
        max_levels: 32,
        promotion_debt: 0,
        max_promotion_debt: 32,
        frontier_merge: None,
        durable_cache: Arc::new(Mutex::new(hydration.cache)),
    })
}
