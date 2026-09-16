//! Durable authenticated Merkle node index.
//!
//! The index is deliberately separate from the object CAS: node commitments
//! are content addressed and can be reused by many roots, while object bytes
//! remain governed by the canonical receiving typestate. A marker appears
//! only after the complete authenticated subtree (including its object
//! receipts) has been committed.

use crate::input_cas::BoundedFileImage;
use backend_engine::ReplicationError;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug)]
pub(super) struct DurableNodeIndex {
    root: Option<PathBuf>,
    // The durable form is deliberately lookup-only. Loading every marker at
    // startup made cold recovery O(number of nodes) in both time and memory.
    // The in-memory form is retained solely for deterministic unit tests.
    complete: Option<BTreeSet<[u8; 32]>>,
}

impl DurableNodeIndex {
    pub(super) fn open(root: Option<PathBuf>) -> Self {
        let complete = root.is_none().then(BTreeSet::new);
        Self { root, complete }
    }

    pub(super) fn contains(&self, digest: [u8; 32]) -> bool {
        if let Some(complete) = self.complete.as_ref() {
            return complete.contains(&digest);
        }
        let Some(root) = self.root.as_ref() else {
            return false;
        };
        let target = root.join(format!(".node-{}", hex(digest)));
        // A marker is complete only when its contents are the exact digest.
        // This is one direct, bounded lookup and never enumerates the index.
        BoundedFileImage::read_optional(&target, 32)
            .is_ok_and(|bytes| bytes.is_some_and(|bytes| bytes.as_slice() == digest.as_slice()))
    }

    pub(super) fn record(&mut self, digest: [u8; 32]) -> Result<(), ReplicationError> {
        if self.contains(digest) {
            return Ok(());
        }
        let Some(root) = self.root.as_ref() else {
            self.complete
                .get_or_insert_with(BTreeSet::new)
                .insert(digest);
            return Ok(());
        };
        let temporary = root.join(format!(".node-{}.part", hex(digest)));
        let target = root.join(format!(".node-{}", hex(digest)));
        // Do not clobber a marker. A crash can leave a valid marker in place;
        // a conflicting marker is corruption and must fail closed.
        if target.exists() {
            return Err(ReplicationError::CorruptFrame);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&digest)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        backend_platform::durability::open_directory(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        Ok(())
    }
}

fn hex(bytes: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn complete_nodes_survive_reopen_and_ignore_partial_markers() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("backend-node-index-{nonce}"));
        fs::create_dir_all(&root).expect("directory");
        let digest = [7; 32];
        let mut index = DurableNodeIndex::open(Some(root.clone()));
        index.record(digest).expect("record");
        assert!(index.contains(digest));
        fs::write(root.join(".node-partial"), digest).expect("partial");
        let reopened = DurableNodeIndex::open(Some(root.clone()));
        assert!(reopened.contains(digest));
        assert!(!reopened.contains([8; 32]));
        let _ = fs::remove_dir_all(root);
    }
}
