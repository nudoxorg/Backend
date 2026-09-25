//! One generation-root commit for both the wire stream and the in-process
//! table.
//!
//! Duplicate intro ids keep the last input occurrence. Every survivor is
//! hashed. [`crate::frontier::ir::project`] then names the payloads to store,
//! and only those entries are encoded into bytes. Location stays on the root
//! row, so a move does not change the payload hash.

use crate::server::registry::blob::creation::BlobBuilder;

/// One symbol kept long enough to build a [`ir::generation::GenerationRoot`].
pub(super) struct StagedSymbol {
    pub(super) intro: ir::change::IntroId,
    pub(super) parent: Option<ir::change::IntroId>,
    pub(super) payload: ir_vcs::wire::OwnedEntryPayload,
}

/// Dual-write a generation root beside the opaque IR blob.
///
/// Payloads are [`ir::content::entry_storage_payload`] bytes, addressed by
/// [`ir::content::entry_storage_hash`]. [`crate::frontier::ir::project`]
/// decides which payloads are queued.
pub(super) fn attach_generation_root(
    builder: &mut BlobBuilder,
    staged: &[StagedSymbol],
    package: Option<&ir::change::PackageLineageId>,
    prior: &[crate::frontier::ir::IrEntryKey],
) -> Result<Vec<crate::frontier::ir::IrEntryKey>, String> {
    let Some(package) = package else {
        return Ok(Vec::new());
    };
    if staged.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<[u8; 32]> = staged
        .iter()
        .map(|symbol| *symbol.intro.as_bytes())
        .collect();
    let mut lowered = Vec::with_capacity(staged.len());
    for index in last_indices(&ids) {
        let symbol = &staged[index];
        lowered.push((
            symbol.intro,
            symbol.parent,
            ir_vcs::lower::lower_payload(&symbol.payload),
        ));
    }
    let borrowed: Vec<_> = lowered
        .iter()
        .map(|(intro, parent, entry)| (*intro, *parent, entry))
        .collect();
    commit_unique(builder, package, &borrowed, prior)
}

/// Dual-write a generation root from a sealed in-process table.
///
/// The table already holds one row per intro id, in intro order.
pub(in crate::server::coordination) fn attach_table_generation_root(
    builder: &mut BlobBuilder,
    table: &ir::apply::PristineIntroTable,
    package: &ir::change::PackageLineageId,
    prior: &[crate::frontier::ir::IrEntryKey],
) -> Result<Vec<crate::frontier::ir::IrEntryKey>, String> {
    if table.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<_> = table
        .iter_sorted()
        .map(|(intro, entry)| (intro, table.parent_of(intro), entry))
        .collect();
    commit_unique(builder, package, &rows, prior)
}

/// Intro ids and storage hashes from the package's current generation root.
///
/// A missing pointer, a manifest with no root, or a root that does not decode
/// yields an empty prior. The next ingest then queues every payload, and the
/// object store dedups by hash.
pub(in crate::server::coordination) async fn prior_entry_keys(
    blobs: &crate::cas::Store<heart::connection::Live>,
    coordinates: &crate::package::Coordinates,
) -> Vec<crate::frontier::ir::IrEntryKey> {
    let manifest = match blobs.get_manifest(coordinates).await {
        Ok(manifest) => manifest,
        Err(_) => return Vec::new(),
    };
    let Some(root_ref) = manifest.root_ref else {
        return Vec::new();
    };
    let bytes = match blobs.get_section(root_ref).await {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let Ok(root) = ir::generation::GenerationRoot::decode(&bytes) else {
        return Vec::new();
    };
    root.entries
        .iter()
        .map(|row| crate::frontier::ir::IrEntryKey {
            intro_id: *row.intro.as_bytes(),
            content_hash: *row.content.as_bytes(),
        })
        .collect()
}

/// Indices of the last occurrence of each id, in id order.
///
/// The radix sort is stable and keeps ascending input index inside a tie, so
/// the last index of a run is the last write. This is the same last-wins rule
/// [`crate::frontier::ir::project`] uses on the keys.
fn last_indices(ids: &[[u8; 32]]) -> Vec<usize> {
    let mut order: Vec<u32> = (0..ids.len() as u32).collect();
    crate::frontier::id_radix::sort_ids(&mut order, |index, byte| ids[index as usize][byte]);
    let mut out = Vec::with_capacity(ids.len());
    let mut run_end = 0;
    while run_end < order.len() {
        let intro = ids[order[run_end] as usize];
        let mut last = run_end;
        run_end += 1;
        while run_end < order.len() && ids[order[run_end] as usize] == intro {
            last = run_end;
            run_end += 1;
        }
        out.push(order[last] as usize);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::last_indices;

    #[test]
    fn duplicate_intros_keep_the_last_write_in_id_order() {
        let alpha = [1u8; 32];
        let beta = [2u8; 32];
        let ids = [beta, alpha, beta, alpha];
        assert_eq!(last_indices(&ids), vec![3, 2]);
    }
}

fn commit_unique(
    builder: &mut BlobBuilder,
    package: &ir::change::PackageLineageId,
    entries: &[(
        ir::change::IntroId,
        Option<ir::change::IntroId>,
        &ir::entry::Entry,
    )],
    prior: &[crate::frontier::ir::IrEntryKey],
) -> Result<Vec<crate::frontier::ir::IrEntryKey>, String> {
    let mut rows = Vec::with_capacity(entries.len());
    let mut keys = Vec::with_capacity(entries.len());
    for (intro, parent, entry) in entries {
        let content = ir::content::entry_storage_hash(entry);
        keys.push(crate::frontier::ir::IrEntryKey {
            intro_id: *intro.as_bytes(),
            content_hash: *content.as_bytes(),
        });
        rows.push(ir::generation::RootEntry {
            intro: *intro,
            content,
            parent: *parent,
            source: entry.sym().source.clone(),
            span: entry.sym().span.clone(),
        });
    }
    let delta = crate::frontier::ir::project(prior, &keys);
    let mut write =
        std::collections::HashSet::with_capacity(delta.added.len() + delta.changed.len());
    for key in delta.added.iter().chain(delta.changed.iter()) {
        write.insert(key.intro_id);
    }
    let payloads = entries
        .iter()
        .zip(&keys)
        .filter_map(|((_, _, entry), key)| {
            write.contains(&key.intro_id).then(|| {
                (
                    ir::change::ContentBlake3::from_raw(key.content_hash),
                    bytes::Bytes::from(ir::content::entry_storage_payload(entry)),
                )
            })
        });
    let root = ir::generation::GenerationRoot::build(package.clone(), rows);
    builder
        .set_generation_root(&root, payloads)
        .map_err(|err| format!("set_generation_root failed: {err}"))?;
    Ok(keys)
}
