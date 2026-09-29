//! Compares persistent row-index batches with a complete rebuild.
#![deny(unsafe_code)]

use std::{error::Error, hint::black_box, time::Instant};

use backend_semantic::ir::row_index::{
    RowFamily, RowPayload, StableRowIndex, StableRowIndexError, StableRowKey,
    StableRowPayloadChange,
};

const ROWS: usize = 16_384;
const PAYLOAD_BYTES: usize = 128;
const ROW_TAG: u8 = 1;
const CASES: [usize; 4] = [0, 1, 8, 64];

fn main() -> Result<(), Box<dyn Error>> {
    let keys = (0..ROWS)
        .map(|ordinal| {
            let mut stable = [0_u8; 32];
            stable[24..].copy_from_slice(
                &u64::try_from(ordinal)
                    .map_err(|_| StableRowIndexError::Overflow)?
                    .to_be_bytes(),
            );
            Ok(StableRowKey::new(RowFamily::Core, stable))
        })
        .collect::<Result<Vec<_>, StableRowIndexError>>()?;
    let source_payloads = (0..ROWS)
        .map(|ordinal| {
            let seed = u8::try_from(ordinal % 251).map_err(|_| StableRowIndexError::Overflow)?;
            Ok(vec![seed; PAYLOAD_BYTES])
        })
        .collect::<Result<Vec<_>, StableRowIndexError>>()?;
    let base_records = records(&keys, &source_payloads)?;
    let (base, base_work) = StableRowIndex::from_sorted_rows_measured(&base_records)?;
    drop(base_records);

    println!(
        "rows={ROWS} payload_bytes_per_row={PAYLOAD_BYTES} full_build_nodes={} full_build_encoded_bytes={} resident_estimate={} row_slots={} referenced_payload_bytes={}",
        base_work.tree.nodes,
        base_work.tree.encoded_bytes,
        base_work.resident.estimated_resident_bytes,
        base_work.resident.row_slot_storage_bytes,
        base_work.resident.referenced_payload_bytes,
    );

    for edits in CASES {
        run_case(&base, &keys, &source_payloads, edits)?;
    }
    Ok(())
}

fn run_case(
    base: &StableRowIndex,
    keys: &[StableRowKey],
    source_payloads: &[Vec<u8>],
    edits: usize,
) -> Result<(), StableRowIndexError> {
    let stride = (source_payloads.len() / edits.max(1)).max(1);
    let changed_at = (0..edits).map(|edit| edit * stride).collect::<Vec<_>>();
    let changed_payloads = (0..edits)
        .map(|edit| {
            let seed = u8::try_from(edit + 1).map_err(|_| StableRowIndexError::Overflow)?;
            let mut bytes = vec![seed; PAYLOAD_BYTES];
            let row = changed_at[edit];
            bytes[0] ^= source_payloads[row][0];
            Ok(bytes)
        })
        .collect::<Result<Vec<_>, StableRowIndexError>>()?;
    let changes = changed_at
        .iter()
        .zip(&changed_payloads)
        .map(|(row, bytes)| StableRowPayloadChange::put(keys[*row], ROW_TAG, bytes))
        .collect::<Vec<_>>();

    let update_start = Instant::now();
    let mut update_work = None;
    for _ in 0..5 {
        let prepared = base.prepare_payload_update(black_box(&changes))?;
        update_work = Some(prepared.work());
        black_box(prepared.commit());
    }
    let update_elapsed = update_start.elapsed();

    let mut target_payloads = source_payloads.to_vec();
    for (row, bytes) in changed_at.into_iter().zip(&changed_payloads) {
        target_payloads[row] = bytes.clone();
    }
    let rebuild_start = Instant::now();
    let mut rebuilt = None;
    let mut rebuild_work = None;
    let mut rebuild_hash_bytes = 0_u64;
    for _ in 0..5 {
        let mut rows = Vec::with_capacity(keys.len());
        let mut hash_bytes = 0_u64;
        for (key, bytes) in keys.iter().copied().zip(&target_payloads) {
            hash_bytes = hash_bytes
                .checked_add(
                    u64::try_from(bytes.len())
                        .map_err(|_| StableRowIndexError::Overflow)?
                        .checked_add(1)
                        .ok_or(StableRowIndexError::Overflow)?,
                )
                .ok_or(StableRowIndexError::Overflow)?;
            rows.push((key, RowPayload::from_tagged_bytes(ROW_TAG, bytes)?));
        }
        let (next, work) = StableRowIndex::from_sorted_rows_measured(&rows)?;
        rebuilt = Some(next);
        rebuild_work = Some(work);
        rebuild_hash_bytes = hash_bytes;
    }
    let rebuild_elapsed = rebuild_start.elapsed();

    let updated = base.prepare_payload_update(&changes)?.commit();
    let rebuilt = rebuilt.ok_or(StableRowIndexError::Overflow)?;
    let rebuild_work = rebuild_work.ok_or(StableRowIndexError::Overflow)?;
    if updated.root() != rebuilt.root() {
        return Err(StableRowIndexError::RootMismatch);
    }
    let updated_resident = updated.memory_usage()?;
    let update_work = update_work.ok_or(StableRowIndexError::Overflow)?;
    println!(
        "edits={edits:>2} update_5x_ns={} full_rebuild_5x_ns={} update_path_nodes={} update_paths_rows={} update_encoded_bytes={} changed_payload_hash_bytes={} full_payload_hash_bytes={} rebuild_nodes={} update_resident_estimate={} rebuild_resident_estimate={}",
        update_elapsed.as_nanos(),
        rebuild_elapsed.as_nanos(),
        update_work.tree.copied_nodes,
        update_work.tree.rows,
        update_work.tree.encoded_bytes,
        update_work.row_payload_hash_bytes,
        rebuild_hash_bytes,
        rebuild_work.tree.nodes,
        updated_resident.estimated_resident_bytes,
        rebuild_work.resident.estimated_resident_bytes,
    );
    Ok(())
}

fn records(
    keys: &[StableRowKey],
    payloads: &[Vec<u8>],
) -> Result<Vec<(StableRowKey, RowPayload)>, StableRowIndexError> {
    keys.iter()
        .copied()
        .zip(payloads)
        .map(|(key, bytes)| Ok((key, RowPayload::from_tagged_bytes(ROW_TAG, bytes)?)))
        .collect()
}
