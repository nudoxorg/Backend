//! Durable root lease publication and epoch recovery for the input CAS.

use super::{File, InputCas, Path, ReplicationError, Schema, Write, fs};

fn write_root_lease(
    directory: &Path,
    root: backend_engine::MerkleRoot,
    epoch: u64,
) -> Result<(), ReplicationError> {
    if epoch == 0 {
        return Err(ReplicationError::InvalidIdentifier);
    }
    let temporary = directory.join(".ROOT_LEASE.part");
    let target = directory.join("ROOT_LEASE");
    let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
    file.write_all(&root.schema().to_be_bytes())
        .and_then(|()| file.write_all(&root.digest().as_bytes()))
        .and_then(|()| file.write_all(&epoch.to_be_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(|_| ReplicationError::Disconnected)?;
    fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
    // Make the authoritative lease name durable before the compatibility
    // ROOT marker is attempted. A crash in the gap then leaves either the
    // old marker with the complete new lease or the old pair, never a marker
    // that claims a root whose lease rename was only in cache.
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ReplicationError::Disconnected)
}

impl<T: Schema> InputCas<T> {
    /// Checks whether the durable lease claim is exactly the supplied checked
    /// root. This is a comparison only; callers must still decide when to
    /// promote the claim into the active typed lease.
    #[must_use]
    pub(crate) fn durable_root_matches(&self, root: backend_engine::MerkleRoot) -> bool {
        self.durable_root_claim
            .is_some_and(|claim| claim.admit_against(root).is_ok())
    }

    /// Binds a durable root claim to the checked root carried by a validated
    /// reconnect offer. Marker bytes never create a checked root themselves.
    pub(crate) fn admit_durable_root(
        &mut self,
        root: backend_engine::MerkleRoot,
    ) -> Result<(), ReplicationError> {
        if let Some(claim) = self.durable_root_claim {
            claim.admit_against(root)?;
        }
        self.admitted_root = Some(root);
        Ok(())
    }

    /// Publishes the authenticated root and its monotonic lifetime lease.
    /// The lease file carries the complete root/epoch pair and is the
    /// restart authority; the short ROOT marker is repaired opportunistically
    /// when a later publication succeeds.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_root(
        &mut self,
        root: backend_engine::MerkleRoot,
    ) -> Result<(), ReplicationError> {
        let epoch = if self.admitted_root == Some(root) && self.root_epoch != 0 {
            self.root_epoch
        } else {
            self.root_epoch
                .checked_add(1)
                .ok_or(ReplicationError::Overflow)?
        };
        let Some(directory) = self.sink.root.as_ref() else {
            self.admitted_root = Some(root);
            self.durable_root_claim = Some(root.to_claim());
            self.root_epoch = epoch;
            self.reclaim_unleased()?;
            if !self.sink.within_budget() {
                return Err(ReplicationError::Backpressure);
            }
            return Ok(());
        };
        // Publish the complete lease before the compatibility marker. If the
        // process stops between the two renames, restart still has one
        // self-authenticating root/epoch pair to recover from.
        write_root_lease(directory, root, epoch)?;
        let temporary = directory.join(".ROOT.part");
        let target = directory.join("ROOT");
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&root.schema().to_be_bytes())
            .and_then(|()| file.write_all(&root.digest().as_bytes()))
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        self.admitted_root = Some(root);
        self.durable_root_claim = Some(root.to_claim());
        self.root_epoch = epoch;
        self.reclaim_unleased()?;
        if !self.sink.within_budget() {
            return Err(ReplicationError::Backpressure);
        }
        Ok(())
    }
}
