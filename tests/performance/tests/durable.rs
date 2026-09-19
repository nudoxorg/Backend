//! Independent structural oracles for the filesystem tree CAS.
//!
//! These tests intentionally observe the durable boundary through public
//! APIs and directory accounting.  They do not use production counters or
//! timing as an oracle: a path copy is accepted only when the number of newly
//! created immutable node files is bounded, and a point read is accepted only
//! when its authenticated path work is logarithmic in the number of rows.

#![forbid(unsafe_code)]

use backend_store::{Change, DurableTree, FileStore, LayoutId, OrderedMap, StoredValue};
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct TempStore {
    path: PathBuf,
}

impl TempStore {
    fn new(label: &str) -> Result<Self, std::io::Error> {
        let path = std::env::temp_dir().join(format!(
            "backend-performance-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn node_bytes(&self) -> Result<(usize, usize), std::io::Error> {
        let mut count = 0usize;
        let mut bytes = 0usize;
        for entry in std::fs::read_dir(self.path.join("nodes"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                count = count.saturating_add(1);
                bytes = bytes
                    .saturating_add(usize::try_from(entry.metadata()?.len()).unwrap_or(usize::MAX));
            }
        }
        Ok((count, bytes))
    }

    fn journal_bytes(&self) -> Result<u64, std::io::Error> {
        Ok(std::fs::metadata(self.path.join("journal"))?.len())
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn value(seed: u8) -> StoredValue {
    StoredValue::new(vec![seed], 1, Vec::new())
}

fn map(rows: usize) -> Result<OrderedMap, backend_store::StoreError> {
    OrderedMap::try_from_iter((0..rows).map(|index| {
        (
            format!("key-{index:08}").into_bytes(),
            value(u8::try_from(index % 251).unwrap_or_default()),
        )
    }))
}

fn boxed_store<T>(
    result: Result<T, backend_store::StoreError>,
) -> Result<T, Box<dyn std::error::Error>> {
    result.map_err(|error| std::io::Error::other(format!("store error: {error:?}")).into())
}

fn node_delta(before: (usize, usize), after: (usize, usize)) -> (usize, usize) {
    (
        after.0.saturating_sub(before.0),
        after.1.saturating_sub(before.1),
    )
}

fn assert_path_read_is_bounded(tree: &DurableTree, rows: usize) -> TestResult {
    let index = rows / 2;
    let key = format!("key-{index:08}").into_bytes();
    let (found, stats) = boxed_store(tree.get_with_stats(&key))?;
    assert_eq!(
        found,
        Some(value(u8::try_from(index % 251).unwrap_or_default()))
    );

    // The canonical tree uses a fixed fanout.  This deliberately leaves a
    // generous constant for small trees and format changes while rejecting a
    // hidden full materialization or scan (which is proportional to `rows`).
    let logarithmic_envelope = usize::try_from(rows.ilog2())
        .unwrap_or(usize::MAX)
        .saturating_mul(8)
        .saturating_add(8);
    assert!(
        stats.nodes_read <= logarithmic_envelope,
        "point read visited too many nodes: rows={rows} stats={stats:?}"
    );
    assert!(stats.bytes_read < rows.saturating_mul(64));
    Ok(())
}

#[test]
fn durable_point_reads_reopen_without_full_tree_materialization() -> TestResult {
    let fixture = TempStore::new("lazy-tree")?;
    let store = boxed_store(FileStore::open(&fixture.path, 8 * 1024 * 1024))?;
    let layout = LayoutId::derive(b"performance-lazy-tree");

    let mut prior_nodes = 0usize;
    let mut prior_reads: Option<usize> = None;
    for rows in [1_024usize, 4_096, 16_384, 65_536] {
        let current = boxed_store(map(rows))?;
        boxed_store(store.publish(&current, layout))?;
        let (nodes, _) = fixture.node_bytes()?;
        assert!(nodes > prior_nodes, "fixture failed to grow at rows={rows}");

        // Reopen only the selected descriptor, then issue one authenticated
        // lookup.  The read counter is independent of filesystem file count.
        let reopened = boxed_store(FileStore::open(&fixture.path, 8 * 1024 * 1024))?;
        let tree = boxed_store(reopened.open_tree())?
            .ok_or_else(|| std::io::Error::other("missing selected tree"))?;
        assert_eq!(tree.root(), current.state_root());
        assert_path_read_is_bounded(&tree, rows)?;
        let probe = rows.saturating_sub(1);
        let (_, stats) = boxed_store(tree.get_with_stats(format!("key-{probe:08}").as_bytes()))?;
        if let Some(previous) = prior_reads {
            assert!(
                stats.nodes_read <= previous.saturating_add(2),
                "point-read work grew faster than tree height: rows={rows} previous={previous} current={stats:?}"
            );
        }
        prior_reads = Some(stats.nodes_read);
        prior_nodes = nodes;
    }
    Ok(())
}

#[test]
fn durable_one_row_update_writes_only_a_path_and_reuses_payload_bytes() -> TestResult {
    let fixture = TempStore::new("path-copy")?;
    let store = boxed_store(FileStore::open(&fixture.path, 8 * 1024 * 1024))?;
    let layout = LayoutId::derive(b"performance-path-copy");
    let base = boxed_store(map(65_536))?;
    boxed_store(store.publish(&base, layout))?;
    let before = fixture.node_bytes()?;
    let journal_before = fixture.journal_bytes()?;

    let index = 32_768usize;
    let edited = boxed_store(base.apply(&[Change {
        key: format!("key-{index:08}").into_bytes(),
        before: Some(value(u8::try_from(index % 251).unwrap_or_default())),
        after: Some(value(249)),
    }]))?;
    boxed_store(store.publish(&edited, layout))?;
    let after = fixture.node_bytes()?;
    let (new_nodes, new_bytes) = node_delta(before, after);

    // A one-row edit can copy one leaf and one node per height.  The bound is
    // intentionally expressed in tree height rather than a machine-dependent
    // timing threshold.  In particular, rewriting every existing node fails
    // by several orders of magnitude at this fixture size.
    let height = usize::try_from(65_536usize.ilog2()).unwrap_or(usize::MAX);
    assert!(
        new_nodes <= height.saturating_mul(4).saturating_add(16),
        "one-row edit wrote too many nodes: new_nodes={new_nodes} height={height}"
    );
    assert!(
        new_bytes < after.1 / 4,
        "one-row edit rewrote too many bytes"
    );
    assert!(fixture.journal_bytes()? > journal_before);

    let reopened = boxed_store(FileStore::open(&fixture.path, 8 * 1024 * 1024))?;
    let tree = boxed_store(reopened.open_tree())?
        .ok_or_else(|| std::io::Error::other("missing edited tree"))?;
    assert_eq!(
        boxed_store(tree.get(format!("key-{index:08}").as_bytes()))?,
        Some(value(249))
    );
    Ok(())
}
