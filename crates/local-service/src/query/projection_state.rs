//! Collection-scoped, crash-safe ownership and membership for Qdrant.
//!
//! One operation lock fences staging, remote publication, pin replacement,
//! retirement, and commit. Membership bytes are immutable content-addressed
//! objects; pin and ledger records contain only bounded descriptors.

use backend_extension_qdrant as qdrant;
use backend_platform::{DirectoryCapability, EntryKind, FileIdentity};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const DIRECTORY: &str = "qdrant-projection";
const VERSION: u8 = 2;
const MEMBERSHIP_MAGIC: &[u8; 8] = b"BMQMEM02";
const LEDGER_MAGIC: &[u8; 8] = b"BMQLED02";
const PIN_MAGIC: &[u8; 8] = b"BMQPIN02";
const OPERATION_LOCK: &str = "collection.operation.lock";
const MAX_ROWS: usize = 65_536;
const MAX_MEMBERSHIP_BYTES: usize = 2_200_000;
const MAX_DESCRIPTORS: usize = 128;
const MAX_OWNER_ENTRIES: usize = 256;
const MAX_OBJECT_ENTRIES: usize = 512;
const MAX_OBJECT_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PROTECTED_MEMBERS: usize = 262_144;
const MAX_STALE_RESIDENCES: usize = 262_144;
const DESCRIPTOR_BYTES: usize = 8 + 1 + 32 + 6 * 32 + 32 + 32 + 4;
const CHECKSUM_BYTES: usize = 32;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Collection-wide durable metadata. The provider scope, not a workspace or
/// recipe, is the coordination boundary because every owner writes one remote
/// Qdrant collection.
pub(super) struct ProjectionState {
    scope: [u8; 32],
    directory: DirectoryCapability,
    owners: DirectoryCapability,
    objects: DirectoryCapability,
    operation_lock_identity: FileIdentity,
    owner_id: [u8; 16],
    owner_lock_identity: FileIdentity,
    owner_pin_lock_identity: FileIdentity,
    _owner_lifecycle_lock: File,
}

/// Exact durable descriptor for one full Qdrant binding and immutable member
/// set. `fields` are workspace, root, recipe, authority, read manifest, and
/// frontier in that order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Descriptor {
    scope: [u8; 32],
    fields: [[u8; 32]; 6],
    identity: qdrant::ProjectionIdentity,
    membership: [u8; 32],
    count: u32,
}

/// Exclusive collection fence. Its borrow prevents state mutation outside the
/// protected operation and it remains held over caller-owned HTTP work.
pub(super) struct ProjectionOperation<'state> {
    state: &'state mut ProjectionState,
    _lock: File,
}

/// Shared OS pin held for the lifetime of an active ANN source.
pub(super) struct ProjectionPin {
    descriptor: Descriptor,
    _lease: PinLease,
}

enum PinLease {
    SharedFile(File),
    #[cfg(test)]
    TestOnly,
}

impl ProjectionPin {
    pub(super) const fn descriptor(&self) -> Descriptor {
        self.descriptor
    }
}

impl ProjectionState {
    /// Opens the configured data directory. Missing durable storage is an
    /// error: publication without a collection-wide pin registry is unsafe.
    pub(super) fn open(scope: [u8; 32]) -> io::Result<Self> {
        let root = std::env::var_os(backend_runtime::DATA_ENV).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Qdrant durable data directory is unset",
            )
        })?;
        let root = Path::new(&root);
        if !root.is_absolute() {
            return Err(invalid_data("Qdrant data directory must be absolute"));
        }
        let data = DirectoryCapability::open(root)?;
        let cache = child_or_create(&data, "cache")?;
        let parent = child_or_create(&cache, DIRECTORY)?;
        Self::open_in_directory_capability(&parent, scope)
    }

    /// Opens one provider subdirectory beneath an existing private parent.
    /// Old per-workspace state is preserved and refused, never reset or moved.
    pub(super) fn open_in_directory(parent: &Path, scope: [u8; 32]) -> io::Result<Self> {
        let parent = DirectoryCapability::open_or_create_private(parent)?;
        Self::open_in_directory_capability(&parent, scope)
    }

    fn open_in_directory_capability(
        parent: &DirectoryCapability,
        scope: [u8; 32],
    ) -> io::Result<Self> {
        refuse_legacy_flat_state(parent)?;
        let scope_name = hexadecimal(&scope);
        let directory = child_or_create(parent, &scope_name)?;
        let owners = child_or_create(&directory, "owners")?;
        let objects = child_or_create(&directory, "objects")?;
        let operation_lock_identity = open_stable_lock(&directory, OPERATION_LOCK)?;
        let operation_file = directory.open_private_file_read_write(OPERATION_LOCK, false)?;
        verify_identity(
            &directory,
            OPERATION_LOCK,
            &operation_file,
            operation_lock_identity,
        )?;
        operation_file.lock()?;
        verify_identity(
            &directory,
            OPERATION_LOCK,
            &operation_file,
            operation_lock_identity,
        )?;
        clean_abandoned_temps(
            &directory,
            &["owners", "objects"],
            &[OPERATION_LOCK, "collection.ledger"],
        )?;
        clean_abandoned_temps(&owners, &[], &[])?;
        clean_abandoned_temps(&objects, &[], &[])?;
        let (owner_id, owner_lock_identity, owner_pin_lock_identity, owner_lifecycle_lock) =
            claim_owner_slot(&owners, scope)?;
        File::unlock(&operation_file)?;
        Ok(Self {
            scope,
            directory,
            owners,
            objects,
            operation_lock_identity,
            owner_id,
            owner_lock_identity,
            owner_pin_lock_identity,
            _owner_lifecycle_lock: owner_lifecycle_lock,
        })
    }

    /// Begins the sole mutation/publication transaction for this collection.
    pub(super) fn begin_operation(&mut self) -> io::Result<ProjectionOperation<'_>> {
        let lock = self
            .directory
            .open_private_file_read_write(OPERATION_LOCK, false)?;
        verify_identity(
            &self.directory,
            OPERATION_LOCK,
            &lock,
            self.operation_lock_identity,
        )?;
        lock.lock()?;
        verify_identity(
            &self.directory,
            OPERATION_LOCK,
            &lock,
            self.operation_lock_identity,
        )?;
        clean_abandoned_temps(
            &self.directory,
            &["owners", "objects"],
            &[OPERATION_LOCK, "collection.ledger"],
        )?;
        clean_abandoned_temps(&self.owners, &[], &[])?;
        clean_abandoned_temps(&self.objects, &[], &[])?;
        Ok(ProjectionOperation {
            state: self,
            _lock: lock,
        })
    }
}

impl ProjectionOperation<'_> {
    /// Rechecks that the stable operation-lock name still identifies the
    /// inode whose exclusive lock this borrowed transaction retains.
    pub(super) fn validate_fence(&self) -> io::Result<()> {
        let lock = self
            .state
            .directory
            .open_private_file_read_write(OPERATION_LOCK, false)?;
        verify_identity(
            &self.state.directory,
            OPERATION_LOCK,
            &lock,
            self.state.operation_lock_identity,
        )
    }

    /// Persists the immutable target object and the union of prior debt, live
    /// owner pins, and this target before any remote upsert can be issued.
    pub(super) fn stage(
        &mut self,
        projection: qdrant::ProjectionBinding,
        row_keys: &[String],
    ) -> io::Result<Descriptor> {
        self.validate_fence()?;
        let mut existing = self.read_ledger()?;
        existing.extend(self.live_pin_descriptors()?);
        let existing = canonical_descriptors(existing, self.state.scope)?;
        self.reclaim_unreferenced_objects(&existing)?;
        let target = self.state.persist_membership(projection, row_keys)?;
        let mut descriptors = self.read_ledger()?;
        descriptors.extend(self.live_pin_descriptors()?);
        descriptors.push(target);
        let descriptors = canonical_descriptors(descriptors, self.state.scope)?;
        self.write_ledger(&descriptors)?;
        Ok(target)
    }

    /// Registers the verified target in this ConfiguredQdrant's stable owner
    /// slot before the caller publishes its replacement ActiveQdrant.
    pub(super) fn replace_owner_pin(
        &mut self,
        descriptor: Descriptor,
    ) -> io::Result<ProjectionPin> {
        self.validate_fence()?;
        validate_descriptor(descriptor, self.state.scope)?;
        self.read_membership_bytes(descriptor)?;
        verify_identity(
            &self.state.owners,
            &owner_lock_name(self.state.owner_id),
            &self.state._owner_lifecycle_lock,
            self.state.owner_lock_identity,
        )?;
        let lock_name = owner_pin_lock_name(self.state.owner_id);
        let pin_lock = self
            .state
            .owners
            .open_private_file_read_write(&lock_name, false)?;
        verify_identity(
            &self.state.owners,
            &lock_name,
            &pin_lock,
            self.state.owner_pin_lock_identity,
        )?;
        pin_lock.lock_shared()?;
        verify_identity(
            &self.state.owners,
            &lock_name,
            &pin_lock,
            self.state.owner_pin_lock_identity,
        )?;
        let encoded = encode_pin(self.state.scope, self.state.owner_id, descriptor);
        atomic_replace(
            &self.state.owners,
            &owner_pin_name(self.state.owner_id),
            &encoded,
        )?;
        Ok(ProjectionPin {
            descriptor,
            _lease: PinLease::SharedFile(pin_lock),
        })
    }

    /// Computes stale point residences while excluding the target and every
    /// currently live pin across workspaces and recipes.
    pub(super) fn stale_residences(
        &mut self,
        target: Descriptor,
    ) -> io::Result<Vec<(qdrant::ProjectionIdentity, Vec<qdrant::PointResidence>)>> {
        self.validate_fence()?;
        validate_descriptor(target, self.state.scope)?;
        let descriptors = self.read_ledger()?;
        let pins = canonical_descriptors(self.live_pin_descriptors()?, self.state.scope)?;
        let mut protected_descriptors = pins.clone();
        protected_descriptors.push(target);
        let protected_descriptors = canonical_descriptors(protected_descriptors, self.state.scope)?;
        let mut protected = HashSet::<(qdrant::ProjectionIdentity, Member)>::new();
        for descriptor in protected_descriptors {
            let bytes = self.read_membership_bytes(descriptor)?;
            for member in decode_membership(&bytes)? {
                let key = (descriptor.identity, member);
                if !protected.contains(&key) && protected.len() >= MAX_PROTECTED_MEMBERS {
                    return Err(invalid_data("Qdrant live residence bound exceeded"));
                }
                protected.insert(key);
            }
        }
        let mut stale =
            BTreeMap::<qdrant::ProjectionIdentity, BTreeSet<qdrant::PointResidence>>::new();
        let mut stale_count = 0_usize;
        for descriptor in descriptors {
            let bytes = self.read_membership_bytes(descriptor)?;
            for member in decode_membership(&bytes)? {
                if protected.contains(&(descriptor.identity, member)) {
                    continue;
                }
                let mut key = [0_u8; 72];
                let residence = qdrant::PointResidence::for_row(
                    descriptor.identity,
                    residence_key(member, &mut key),
                )
                .map_err(|_| invalid_data("invalid Qdrant membership key"))?;
                if stale
                    .entry(descriptor.identity)
                    .or_default()
                    .insert(residence)
                {
                    stale_count += 1;
                    if stale_count > MAX_STALE_RESIDENCES {
                        return Err(invalid_data("Qdrant stale residence bound exceeded"));
                    }
                }
            }
        }
        Ok(stale
            .into_iter()
            .map(|(identity, residences)| (identity, residences.into_iter().collect()))
            .collect())
    }

    /// Commits only the exact target and all currently live owner pins after
    /// every unpinned residence has been retired and verified by Qdrant.
    pub(super) fn commit_verified(&mut self, target: Descriptor) -> io::Result<()> {
        self.validate_fence()?;
        validate_descriptor(target, self.state.scope)?;
        let prior = self.read_ledger()?;
        let mut retained = self.live_pin_descriptors()?;
        retained.push(target);
        let retained = canonical_descriptors(retained, self.state.scope)?;
        self.remove_inactive_pin_descriptors(&prior, &retained)?;
        self.validate_fence()?;
        self.write_ledger(&retained)?;
        self.reclaim_unreferenced_objects(&retained)?;
        Ok(())
    }

    fn read_ledger(&self) -> io::Result<Vec<Descriptor>> {
        let bytes = read_bounded(
            &self.state.directory,
            "collection.ledger",
            ledger_max_bytes(),
        )?;
        match bytes {
            None => {
                if self
                    .state
                    .owners
                    .entries(MAX_OWNER_ENTRIES)?
                    .iter()
                    .any(|entry| {
                        entry
                            .name
                            .to_str()
                            .is_some_and(|name| name.ends_with(".pin"))
                    })
                {
                    return Err(invalid_data(
                        "Qdrant pin descriptor exists without its durable ledger",
                    ));
                }
                Ok(Vec::new())
            }
            Some(bytes) => decode_ledger(&bytes, self.state.scope),
        }
    }

    fn write_ledger(&self, descriptors: &[Descriptor]) -> io::Result<()> {
        let bytes = encode_ledger(descriptors, self.state.scope)?;
        atomic_replace(&self.state.directory, "collection.ledger", &bytes)
    }

    fn live_pin_descriptors(&self) -> io::Result<Vec<Descriptor>> {
        let ledger = self.read_ledger()?;
        let entries = self.state.owners.entries(MAX_OWNER_ENTRIES)?;
        let entry_count = entries.len();
        let mut descriptors = Vec::new();
        let mut owner_ids = BTreeSet::new();
        let mut pin_lock_ids = BTreeSet::new();
        let mut pin_ids = BTreeSet::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                return Err(invalid_data("unexpected Qdrant owner entry kind"));
            }
            let Some(name) = entry.name.to_str() else {
                return Err(invalid_data("non-Unicode Qdrant owner entry"));
            };
            if let Some(owner_hex) = name.strip_suffix(".owner.lock") {
                owner_ids.insert(parse_hex_16(owner_hex)?);
            } else if let Some(owner_hex) = name.strip_suffix(".pin.lock") {
                pin_lock_ids.insert(parse_hex_16(owner_hex)?);
            } else if let Some(owner_hex) = name.strip_suffix(".pin") {
                pin_ids.insert(parse_hex_16(owner_hex)?);
            } else {
                return Err(invalid_data("unknown Qdrant owner metadata"));
            }
        }
        if owner_ids.len() > MAX_OWNER_ENTRIES / 3
            || pin_lock_ids.len() > MAX_OWNER_ENTRIES / 3
            || pin_ids.len() > MAX_OWNER_ENTRIES / 2
            || pin_lock_ids.iter().any(|owner| !owner_ids.contains(owner))
            || pin_ids.iter().any(|owner| !pin_lock_ids.contains(owner))
            || owner_ids.iter().any(|owner| !pin_lock_ids.contains(owner))
        {
            return Err(invalid_data("Qdrant owner pin inventory is inconsistent"));
        }
        for owner in owner_ids {
            let lifecycle_name = owner_lock_name(owner);
            let lifecycle = self
                .state
                .owners
                .open_private_file_read_write(&lifecycle_name, false)?;
            let lifecycle_identity = FileIdentity::of_file(&lifecycle)?;
            let expected_lifecycle_identity = if owner == self.state.owner_id {
                self.state.owner_lock_identity
            } else {
                lifecycle_identity
            };
            verify_identity(
                &self.state.owners,
                &lifecycle_name,
                &lifecycle,
                lifecycle_identity,
            )?;
            if lifecycle_identity != expected_lifecycle_identity {
                return Err(invalid_data("Qdrant owner lock identity changed"));
            }
            let lifecycle_lock = lifecycle.try_lock();
            verify_identity(
                &self.state.owners,
                &lifecycle_name,
                &lifecycle,
                lifecycle_identity,
            )?;
            match lifecycle_lock {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
            let pin_name = owner_pin_lock_name(owner);
            let pin_lock = self
                .state
                .owners
                .open_private_file_read_write(&pin_name, false)?;
            let pin_identity = FileIdentity::of_file(&pin_lock)?;
            let expected_pin_identity = if owner == self.state.owner_id {
                self.state.owner_pin_lock_identity
            } else {
                pin_identity
            };
            verify_identity(&self.state.owners, &pin_name, &pin_lock, pin_identity)?;
            if pin_identity != expected_pin_identity {
                return Err(invalid_data("Qdrant active pin lock identity changed"));
            }
            let pin_lock_result = pin_lock.try_lock();
            verify_identity(&self.state.owners, &pin_name, &pin_lock, pin_identity)?;
            let active_pin = match pin_lock_result {
                Ok(()) => false,
                Err(std::fs::TryLockError::WouldBlock) => true,
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            };
            if active_pin {
                let pin = read_bounded(&self.state.owners, &owner_pin_name(owner), PIN_BYTES)?
                    .ok_or_else(|| invalid_data("live Qdrant pin has no descriptor"))?;
                let descriptor = decode_pin(&pin, self.state.scope, owner)?;
                validate_descriptor(descriptor, self.state.scope)?;
                if !ledger.contains(&descriptor) {
                    return Err(invalid_data(
                        "live Qdrant pin is absent from the durable membership ledger",
                    ));
                }
                descriptors.push(descriptor);
                continue;
            }
            if pin_ids.contains(&owner) {
                let pin = read_bounded(&self.state.owners, &owner_pin_name(owner), PIN_BYTES)?
                    .ok_or_else(|| invalid_data("Qdrant owner pin disappeared"))?;
                let descriptor = decode_pin(&pin, self.state.scope, owner)?;
                validate_descriptor(descriptor, self.state.scope)?;
                if !ledger.contains(&descriptor) {
                    return Err(invalid_data(
                        "inactive Qdrant pin is absent from the durable membership ledger",
                    ));
                }
            }
        }
        if entry_count > MAX_OWNER_ENTRIES || descriptors.len() > MAX_DESCRIPTORS {
            return Err(invalid_data("Qdrant live pin inventory limit exceeded"));
        }
        Ok(descriptors)
    }

    /// Removes only inactive pin descriptors whose membership is still
    /// represented by the durable pre-commit ledger or retained exact target.
    /// The staged ledger remains authoritative until every stale delete has
    /// already succeeded, so a crash or cleanup error leaves retryable debt.
    fn remove_inactive_pin_descriptors(
        &self,
        prior: &[Descriptor],
        retained: &[Descriptor],
    ) -> io::Result<()> {
        let entries = self.state.owners.entries(MAX_OWNER_ENTRIES)?;
        let mut owner_ids = BTreeSet::new();
        let mut pin_lock_ids = BTreeSet::new();
        let mut pin_ids = BTreeSet::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                return Err(invalid_data("unexpected Qdrant owner entry kind"));
            }
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| invalid_data("non-Unicode Qdrant owner entry"))?;
            if let Some(owner_hex) = name.strip_suffix(".owner.lock") {
                owner_ids.insert(parse_hex_16(owner_hex)?);
            } else if let Some(owner_hex) = name.strip_suffix(".pin.lock") {
                pin_lock_ids.insert(parse_hex_16(owner_hex)?);
            } else if let Some(owner_hex) = name.strip_suffix(".pin") {
                pin_ids.insert(parse_hex_16(owner_hex)?);
            } else {
                return Err(invalid_data("unknown Qdrant owner metadata"));
            }
        }
        if owner_ids.len() > MAX_OWNER_ENTRIES / 3
            || pin_lock_ids.len() > MAX_OWNER_ENTRIES / 3
            || pin_ids.len() > MAX_OWNER_ENTRIES / 2
            || pin_lock_ids.iter().any(|owner| !owner_ids.contains(owner))
            || pin_ids.iter().any(|owner| !pin_lock_ids.contains(owner))
            || owner_ids.iter().any(|owner| !pin_lock_ids.contains(owner))
        {
            return Err(invalid_data("Qdrant owner pin inventory is inconsistent"));
        }
        for owner in pin_ids {
            let lock_name = owner_pin_lock_name(owner);
            let lock = self
                .state
                .owners
                .open_private_file_read_write(&lock_name, false)?;
            let identity = FileIdentity::of_file(&lock)?;
            let expected_identity = if owner == self.state.owner_id {
                self.state.owner_pin_lock_identity
            } else {
                identity
            };
            verify_identity(&self.state.owners, &lock_name, &lock, identity)?;
            if identity != expected_identity {
                return Err(invalid_data("Qdrant active pin lock identity changed"));
            }
            let lock_result = lock.try_lock();
            verify_identity(&self.state.owners, &lock_name, &lock, identity)?;
            match lock_result {
                Ok(()) => {
                    let pin_name = owner_pin_name(owner);
                    let bytes = read_bounded(&self.state.owners, &pin_name, PIN_BYTES)?
                        .ok_or_else(|| invalid_data("Qdrant owner pin disappeared"))?;
                    let descriptor = decode_pin(&bytes, self.state.scope, owner)?;
                    validate_descriptor(descriptor, self.state.scope)?;
                    if !prior.contains(&descriptor) && !retained.contains(&descriptor) {
                        return Err(invalid_data(
                            "inactive Qdrant pin is absent from the durable membership ledger",
                        ));
                    }
                    self.state.owners.remove_file(&pin_name)?;
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_membership_bytes(&self, descriptor: Descriptor) -> io::Result<Vec<u8>> {
        validate_descriptor(descriptor, self.state.scope)?;
        let name = object_name(descriptor.membership);
        let bytes = read_bounded(&self.state.objects, &name, MAX_MEMBERSHIP_BYTES)?
            .ok_or_else(|| invalid_data("Qdrant membership object is missing"))?;
        let expected_hash = membership_hash(&bytes);
        if expected_hash != descriptor.membership {
            return Err(invalid_data("Qdrant membership object digest mismatch"));
        }
        let members = decode_membership(&bytes)?;
        if members.count() != usize::try_from(descriptor.count).unwrap_or(usize::MAX) {
            return Err(invalid_data("Qdrant membership count mismatch"));
        }
        Ok(bytes)
    }

    fn reclaim_unreferenced_objects(&self, retained: &[Descriptor]) -> io::Result<()> {
        let referenced = retained
            .iter()
            .map(|descriptor| object_name(descriptor.membership))
            .collect::<HashSet<_>>();
        for entry in self.state.objects.entries(MAX_OBJECT_ENTRIES)? {
            if entry.kind != EntryKind::File {
                return Err(invalid_data("unexpected Qdrant object entry kind"));
            }
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| invalid_data("non-Unicode Qdrant object name"))?;
            if name.ends_with(".tmp") {
                self.state.objects.remove_file(name)?;
                continue;
            }
            if !name.ends_with(".obj") {
                return Err(invalid_data("unknown Qdrant membership object"));
            }
            if !referenced.contains(name) {
                let bytes = read_bounded(&self.state.objects, name, MAX_MEMBERSHIP_BYTES)?
                    .ok_or_else(|| invalid_data("Qdrant membership object disappeared"))?;
                let digest = membership_hash(&bytes);
                if object_name(digest) != name {
                    return Err(invalid_data("unreferenced Qdrant object is corrupt"));
                }
                let _ = decode_membership(&bytes)?.count();
                self.state.objects.remove_file(name)?;
            }
        }
        Ok(())
    }
}

impl ProjectionPin {
    /// A test-only seam for transport tests that do not exercise persistence.
    /// Production activation never constructs a pin without the OS lease.
    #[cfg(test)]
    pub(super) fn test_only(binding: qdrant::Binding) -> Self {
        let fields = binding_fields(binding);
        let identity = qdrant::ProjectionIdentity::from_canonical_fields(
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        );
        let bytes = encode_membership(&[]).expect("empty membership is bounded");
        Self {
            descriptor: Descriptor {
                scope: [0; 32],
                fields,
                identity,
                membership: membership_hash(&bytes),
                count: 0,
            },
            _lease: PinLease::TestOnly,
        }
    }
}

impl ProjectionState {
    fn persist_membership(
        &self,
        projection: qdrant::ProjectionBinding,
        row_keys: &[String],
    ) -> io::Result<Descriptor> {
        let members = admit_members(row_keys)?;
        let bytes = encode_membership(&members)?;
        let digest = membership_hash(&bytes);
        let name = object_name(digest);
        match self.objects.open_file_read(&name) {
            Ok(existing) => {
                drop(existing);
                let read = read_bounded(&self.objects, &name, MAX_MEMBERSHIP_BYTES)?
                    .ok_or_else(|| invalid_data("Qdrant object disappeared"))?;
                if read != bytes {
                    return Err(invalid_data("Qdrant content address collision"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let entries = self.objects.entries(MAX_OBJECT_ENTRIES)?;
                let mut total = 0_u64;
                for entry in &entries {
                    if entry.kind != EntryKind::File {
                        return Err(invalid_data("unexpected membership object kind"));
                    }
                    let name = entry
                        .name
                        .to_str()
                        .ok_or_else(|| invalid_data("non-Unicode membership object name"))?;
                    let object = self.objects.open_private_file(name)?;
                    total = total
                        .checked_add(object.metadata()?.len())
                        .ok_or_else(state_limit)?;
                }
                let next_total = total
                    .checked_add(u64::try_from(bytes.len()).map_err(|_| state_limit())?)
                    .ok_or_else(state_limit)?;
                if entries.len() >= MAX_OBJECT_ENTRIES || next_total > MAX_OBJECT_TOTAL_BYTES {
                    return Err(invalid_data("Qdrant membership object inventory is full"));
                }
                atomic_create(&self.objects, &name, &bytes)?;
            }
            Err(error) => return Err(error),
        }
        let fields = binding_fields(projection.binding());
        Ok(Descriptor {
            scope: self.scope,
            fields,
            identity: projection.identity(),
            membership: digest,
            count: u32::try_from(members.len())
                .map_err(|_| invalid_data("Qdrant member count overflow"))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Member {
    kind: u8,
    digest: [u8; 32],
}

impl Member {
    fn stable_key(self) -> &'static str {
        // This is used only as a conceptual marker; residence_key below
        // provides the stack-backed canonical row string.
        match self.kind {
            0 => "object",
            1 => "package",
            2 => "symbol",
            _ => unreachable!("validated member kind"),
        }
    }
}

fn residence_key(member: Member, scratch: &mut [u8; 72]) -> &str {
    let label = member.stable_key().as_bytes();
    let mut length = 0;
    scratch[..label.len()].copy_from_slice(label);
    length += label.len();
    scratch[length] = b':';
    length += 1;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in member.digest {
        scratch[length] = HEX[usize::from(byte >> 4)];
        scratch[length + 1] = HEX[usize::from(byte & 0x0f)];
        length += 2;
    }
    std::str::from_utf8(&scratch[..length]).expect("canonical row key is ASCII")
}

fn admit_members(row_keys: &[String]) -> io::Result<Vec<Member>> {
    if row_keys.len() > MAX_ROWS {
        return Err(invalid_data("Qdrant membership row limit exceeded"));
    }
    let mut members = Vec::new();
    members
        .try_reserve_exact(row_keys.len())
        .map_err(|_| io::Error::other("Qdrant membership allocation failed"))?;
    for row in row_keys {
        let (kind, digest) = row
            .split_once(':')
            .ok_or_else(|| invalid_data("malformed Qdrant row key"))?;
        let kind = match kind {
            "object" => 0,
            "package" => 1,
            "symbol" => 2,
            _ => return Err(invalid_data("unknown Qdrant row kind")),
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid_data("noncanonical Qdrant row digest"));
        }
        let mut raw = [0_u8; 32];
        for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
            raw[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
        }
        members.push(Member { kind, digest: raw });
    }
    members.sort_unstable();
    members.dedup();
    Ok(members)
}

fn encode_membership(members: &[Member]) -> io::Result<Vec<u8>> {
    if members.len() > MAX_ROWS || members.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_data("noncanonical Qdrant membership"));
    }
    let kinds_len = members.len().checked_add(3).ok_or_else(state_limit)? / 4;
    let capacity = 8_usize
        .checked_add(1 + 4)
        .and_then(|size| size.checked_add(kinds_len))
        .and_then(|size| size.checked_add(members.len().checked_mul(32)?))
        .and_then(|size| size.checked_add(CHECKSUM_BYTES))
        .filter(|size| *size <= MAX_MEMBERSHIP_BYTES)
        .ok_or_else(state_limit)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| io::Error::other("Qdrant membership allocation failed"))?;
    output.extend_from_slice(MEMBERSHIP_MAGIC);
    output.push(VERSION);
    output.extend_from_slice(
        &u32::try_from(members.len())
            .map_err(|_| state_limit())?
            .to_be_bytes(),
    );
    let kinds_start = output.len();
    output.resize(kinds_start + kinds_len, 0);
    for (index, member) in members.iter().enumerate() {
        if member.kind > 2 {
            return Err(invalid_data("invalid Qdrant row kind"));
        }
        output[kinds_start + index / 4] |= member.kind << ((index % 4) * 2);
    }
    for member in members {
        output.extend_from_slice(&member.digest);
    }
    append_checksum("backend.local-service.qdrant-membership.v2", &mut output);
    Ok(output)
}

struct Membership<'a> {
    bytes: &'a [u8],
    count: usize,
    lane_start: usize,
    next_index: usize,
}

impl Membership<'_> {
    fn count(&self) -> usize {
        self.count
    }

    fn at(&self, index: usize) -> Member {
        let kind = (self.bytes[13 + index / 4] >> ((index % 4) * 2)) & 0b11;
        let offset = self.lane_start + index * 32;
        let digest = self.bytes[offset..offset + 32]
            .try_into()
            .expect("validated digest lane has fixed width");
        Member { kind, digest }
    }
}

impl Iterator for Membership<'_> {
    type Item = Member;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 {
            return None;
        }
        let index = self.next_index;
        if index >= self.count {
            return None;
        }
        self.next_index += 1;
        Some(self.at(index))
    }
}

fn decode_membership(bytes: &[u8]) -> io::Result<Membership<'_>> {
    if bytes.len() < 8 + 1 + 4 + CHECKSUM_BYTES
        || bytes.len() > MAX_MEMBERSHIP_BYTES
        || &bytes[..8] != MEMBERSHIP_MAGIC
        || bytes[8] != VERSION
    {
        return Err(invalid_data("invalid Qdrant membership header"));
    }
    check_checksum("backend.local-service.qdrant-membership.v2", bytes)?;
    let count = usize::try_from(u32::from_be_bytes(
        bytes[9..13]
            .try_into()
            .map_err(|_| invalid_data("invalid member count"))?,
    ))
    .map_err(|_| state_limit())?;
    if count > MAX_ROWS {
        return Err(invalid_data("Qdrant membership count limit exceeded"));
    }
    let kinds_len = count.checked_add(3).ok_or_else(state_limit)? / 4;
    let lane_start = 13_usize.checked_add(kinds_len).ok_or_else(state_limit)?;
    let checksum_at = bytes.len() - CHECKSUM_BYTES;
    let expected = lane_start
        .checked_add(count.checked_mul(32).ok_or_else(state_limit)?)
        .ok_or_else(state_limit)?;
    if expected != checksum_at {
        return Err(invalid_data("invalid Qdrant membership size"));
    }
    if count % 4 != 0 && kinds_len > 0 {
        let used = (count % 4) * 2;
        if bytes[12 + kinds_len] & !((1_u8 << used) - 1) != 0 {
            return Err(invalid_data("nonzero Qdrant membership padding"));
        }
    }
    let members = Membership {
        bytes,
        count,
        lane_start,
        next_index: 0,
    };
    let mut prior = None;
    for index in 0..count {
        let member = members.at(index);
        if member.kind > 2 {
            return Err(invalid_data("invalid packed Qdrant row kind"));
        }
        if prior.is_some_and(|previous| previous >= member) {
            return Err(invalid_data("unordered Qdrant membership"));
        }
        prior = Some(member);
    }
    Ok(members)
}

fn binding_fields(binding: qdrant::Binding) -> [[u8; 32]; 6] {
    [
        *binding.workspace.as_bytes(),
        *binding.root.as_bytes(),
        *binding.recipe.as_bytes(),
        *binding.authority.as_bytes(),
        *binding.read_manifest.as_bytes(),
        *binding.frontier().as_bytes(),
    ]
}

fn canonical_descriptors(
    mut descriptors: Vec<Descriptor>,
    scope: [u8; 32],
) -> io::Result<Vec<Descriptor>> {
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid_data("Qdrant descriptor limit exceeded"));
    }
    for descriptor in &descriptors {
        validate_descriptor(*descriptor, scope)?;
    }
    descriptors.sort_by_key(|descriptor| (descriptor.identity, descriptor.membership));
    descriptors.dedup();
    for pair in descriptors.windows(2) {
        if pair[0].identity == pair[1].identity && pair[0].membership != pair[1].membership {
            return Err(invalid_data(
                "one Qdrant binding has conflicting membership",
            ));
        }
    }
    Ok(descriptors)
}

fn validate_descriptor(descriptor: Descriptor, scope: [u8; 32]) -> io::Result<()> {
    let identity = qdrant::ProjectionIdentity::from_canonical_fields(
        descriptor.fields[0],
        descriptor.fields[1],
        descriptor.fields[2],
        descriptor.fields[3],
        descriptor.fields[4],
        descriptor.fields[5],
    );
    if descriptor.scope != scope || descriptor.identity != identity {
        return Err(invalid_data("Qdrant descriptor binding mismatch"));
    }
    if usize::try_from(descriptor.count).unwrap_or(usize::MAX) > MAX_ROWS {
        return Err(invalid_data("Qdrant descriptor count limit exceeded"));
    }
    Ok(())
}

fn encode_descriptor(descriptor: Descriptor, out: &mut Vec<u8>) {
    out.extend_from_slice(b"BMQDESC2");
    out.push(VERSION);
    out.extend_from_slice(&descriptor.scope);
    for field in descriptor.fields {
        out.extend_from_slice(&field);
    }
    out.extend_from_slice(descriptor.identity.as_bytes());
    out.extend_from_slice(&descriptor.membership);
    out.extend_from_slice(&descriptor.count.to_be_bytes());
}

fn decode_descriptor(bytes: &[u8]) -> io::Result<Descriptor> {
    if bytes.len() != DESCRIPTOR_BYTES || &bytes[..8] != b"BMQDESC2" || bytes[8] != VERSION {
        return Err(invalid_data("invalid Qdrant descriptor"));
    }
    let mut offset = 9;
    let scope = take32(bytes, &mut offset)?;
    let mut fields = [[0_u8; 32]; 6];
    for field in &mut fields {
        *field = take32(bytes, &mut offset)?;
    }
    let stored_identity = take32(bytes, &mut offset)?;
    let membership = take32(bytes, &mut offset)?;
    let count = u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .map_err(|_| invalid_data("invalid Qdrant descriptor count"))?,
    );
    if usize::try_from(count).unwrap_or(usize::MAX) > MAX_ROWS {
        return Err(invalid_data("Qdrant descriptor count limit exceeded"));
    }
    let identity = qdrant::ProjectionIdentity::from_canonical_fields(
        fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
    );
    if identity.as_bytes() != &stored_identity {
        return Err(invalid_data("Qdrant descriptor identity mismatch"));
    }
    Ok(Descriptor {
        scope,
        fields,
        identity,
        membership,
        count,
    })
}

fn encode_ledger(descriptors: &[Descriptor], scope: [u8; 32]) -> io::Result<Vec<u8>> {
    let descriptors = canonical_descriptors(descriptors.to_vec(), scope)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(8 + 1 + 4 + descriptors.len() * DESCRIPTOR_BYTES + CHECKSUM_BYTES)
        .map_err(|_| io::Error::other("Qdrant ledger allocation failed"))?;
    bytes.extend_from_slice(LEDGER_MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(
        &u32::try_from(descriptors.len())
            .map_err(|_| state_limit())?
            .to_be_bytes(),
    );
    for descriptor in descriptors {
        encode_descriptor(descriptor, &mut bytes);
    }
    append_checksum("backend.local-service.qdrant-ledger.v2", &mut bytes);
    Ok(bytes)
}

fn decode_ledger(bytes: &[u8], scope: [u8; 32]) -> io::Result<Vec<Descriptor>> {
    if bytes.len() < 8 + 1 + 4 + CHECKSUM_BYTES
        || &bytes[..8] != LEDGER_MAGIC
        || bytes[8] != VERSION
    {
        return Err(invalid_data("invalid Qdrant ledger header"));
    }
    check_checksum("backend.local-service.qdrant-ledger.v2", bytes)?;
    let count = usize::try_from(u32::from_be_bytes(
        bytes[9..13]
            .try_into()
            .map_err(|_| invalid_data("invalid Qdrant ledger count"))?,
    ))
    .map_err(|_| state_limit())?;
    if count > MAX_DESCRIPTORS || 13 + count * DESCRIPTOR_BYTES + CHECKSUM_BYTES != bytes.len() {
        return Err(invalid_data("invalid Qdrant ledger size"));
    }
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(count)
        .map_err(|_| io::Error::other("Qdrant ledger allocation failed"))?;
    let mut offset = 13;
    for _ in 0..count {
        descriptors.push(decode_descriptor(
            &bytes[offset..offset + DESCRIPTOR_BYTES],
        )?);
        offset += DESCRIPTOR_BYTES;
    }
    canonical_descriptors(descriptors, scope)
}

const PIN_BYTES: usize = 8 + 1 + 16 + DESCRIPTOR_BYTES + CHECKSUM_BYTES;

fn encode_pin(scope: [u8; 32], owner: [u8; 16], descriptor: Descriptor) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PIN_BYTES);
    bytes.extend_from_slice(PIN_MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&owner);
    encode_descriptor(descriptor, &mut bytes);
    append_checksum("backend.local-service.qdrant-pin.v2", &mut bytes);
    debug_assert_eq!(bytes.len(), PIN_BYTES);
    debug_assert_eq!(descriptor.scope, scope);
    bytes
}

fn decode_pin(bytes: &[u8], scope: [u8; 32], owner: [u8; 16]) -> io::Result<Descriptor> {
    if bytes.len() != PIN_BYTES
        || &bytes[..8] != PIN_MAGIC
        || bytes[8] != VERSION
        || bytes[9..25] != owner
    {
        return Err(invalid_data("invalid Qdrant pin header"));
    }
    check_checksum("backend.local-service.qdrant-pin.v2", bytes)?;
    let descriptor = decode_descriptor(&bytes[25..25 + DESCRIPTOR_BYTES])?;
    if descriptor.scope != scope {
        return Err(invalid_data("Qdrant pin provider mismatch"));
    }
    Ok(descriptor)
}

fn append_checksum(domain: &'static str, bytes: &mut Vec<u8>) {
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(bytes);
    bytes.extend_from_slice(hasher.finalize().as_bytes());
}

fn check_checksum(domain: &'static str, bytes: &[u8]) -> io::Result<()> {
    let checksum_at = bytes
        .len()
        .checked_sub(CHECKSUM_BYTES)
        .ok_or_else(state_data)?;
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(&bytes[..checksum_at]);
    if bytes[checksum_at..] != *hasher.finalize().as_bytes() {
        return Err(invalid_data("Qdrant state checksum mismatch"));
    }
    Ok(())
}

fn read_bounded(
    directory: &DirectoryCapability,
    name: &str,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let file = match directory.open_private_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(invalid_data("Qdrant state file exceeds its bound"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(metadata.len()).map_err(|_| state_limit())?)
        .map_err(|_| io::Error::other("Qdrant state allocation failed"))?;
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid_data("Qdrant state file exceeds its bound"));
    }
    Ok(Some(bytes))
}

fn atomic_replace(
    directory: &DirectoryCapability,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let temporary = temporary_name(destination);
    let mut file = directory.create_file_exclusive(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = directory.remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    match directory.rename_with_outcome(&temporary, destination, true) {
        Ok(()) => Ok(()),
        Err(error) => {
            if !error.committed() {
                let _ = directory.remove_file(&temporary);
            }
            Err(error.into_io_error())
        }
    }
}

fn atomic_create(
    directory: &DirectoryCapability,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let temporary = temporary_name(destination);
    let mut file = directory.create_file_exclusive(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = directory.remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    match directory.rename_with_outcome(&temporary, destination, false) {
        Ok(()) => Ok(()),
        Err(error) => {
            if !error.committed() {
                let _ = directory.remove_file(&temporary);
            }
            Err(error.into_io_error())
        }
    }
}

fn temporary_name(destination: &str) -> String {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "tmp-{}-{sequence}-{}.tmp",
        std::process::id(),
        &blake3::hash(destination.as_bytes()).to_hex()[..12]
    )
}

fn open_stable_lock(directory: &DirectoryCapability, name: &str) -> io::Result<FileIdentity> {
    match directory.create_file_exclusive(name) {
        Ok(file) => {
            file.sync_all()?;
            directory.sync_all()?;
            let identity = FileIdentity::of_file(&file)?;
            verify_identity(directory, name, &file, identity)?;
            Ok(identity)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let file = directory.open_private_file_read_write(name, false)?;
            let identity = FileIdentity::of_file(&file)?;
            verify_identity(directory, name, &file, identity)?;
            Ok(identity)
        }
        Err(error) => Err(error),
    }
}

fn create_stable_lock(
    directory: &DirectoryCapability,
    name: &str,
) -> io::Result<(File, FileIdentity)> {
    let file = directory.create_file_exclusive(name)?;
    file.sync_all()?;
    directory.sync_all()?;
    let identity = FileIdentity::of_file(&file)?;
    verify_identity(directory, name, &file, identity)?;
    Ok((file, identity))
}

/// Reuses only an owner slot whose lifecycle and active-view locks can both be
/// acquired exclusively. Lock files themselves are never unlinked or replaced.
fn claim_owner_slot(
    owners: &DirectoryCapability,
    scope: [u8; 32],
) -> io::Result<([u8; 16], FileIdentity, FileIdentity, File)> {
    let entries = owners.entries(MAX_OWNER_ENTRIES)?;
    let mut owner_ids = BTreeSet::new();
    let mut pin_lock_ids = BTreeSet::new();
    let mut pin_ids = BTreeSet::new();
    for entry in &entries {
        if entry.kind != EntryKind::File {
            return Err(invalid_data("unexpected Qdrant owner entry kind"));
        }
        let name = entry
            .name
            .to_str()
            .ok_or_else(|| invalid_data("non-Unicode Qdrant owner entry"))?;
        if let Some(hex) = name.strip_suffix(".owner.lock") {
            owner_ids.insert(parse_hex_16(hex)?);
        } else if let Some(hex) = name.strip_suffix(".pin.lock") {
            pin_lock_ids.insert(parse_hex_16(hex)?);
        } else if let Some(hex) = name.strip_suffix(".pin") {
            pin_ids.insert(parse_hex_16(hex)?);
        } else {
            return Err(invalid_data("unknown Qdrant owner metadata"));
        }
    }
    if pin_ids.iter().any(|owner| !owner_ids.contains(owner))
        || pin_lock_ids.iter().any(|owner| !owner_ids.contains(owner))
        || pin_ids.iter().any(|owner| !pin_lock_ids.contains(owner))
    {
        return Err(invalid_data("Qdrant owner slot inventory is inconsistent"));
    }
    for owner in owner_ids {
        let lifecycle_name = owner_lock_name(owner);
        let lifecycle = owners.open_private_file_read_write(&lifecycle_name, false)?;
        let lifecycle_identity = FileIdentity::of_file(&lifecycle)?;
        verify_identity(owners, &lifecycle_name, &lifecycle, lifecycle_identity)?;
        let lifecycle_lock = lifecycle.try_lock();
        verify_identity(owners, &lifecycle_name, &lifecycle, lifecycle_identity)?;
        match lifecycle_lock {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                if !pin_lock_ids.contains(&owner) {
                    return Err(invalid_data("live Qdrant owner has no stable pin lock"));
                }
                continue;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        let pin_name = owner_pin_lock_name(owner);
        let (pin_file, pin_identity) = match owners.open_private_file_read_write(&pin_name, false) {
            Ok(pin_file) => {
                let identity = FileIdentity::of_file(&pin_file)?;
                verify_identity(owners, &pin_name, &pin_file, identity)?;
                (pin_file, identity)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (file, identity) = create_stable_lock(owners, &pin_name)?;
                (file, identity)
            }
            Err(error) => return Err(error),
        };
        let pin_lock_result = pin_file.try_lock();
        verify_identity(owners, &pin_name, &pin_file, pin_identity)?;
        match pin_lock_result {
            Ok(()) => {
                lifecycle.lock_shared()?;
                verify_identity(owners, &lifecycle_name, &lifecycle, lifecycle_identity)?;
                return Ok((owner, lifecycle_identity, pin_identity, lifecycle));
            }
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    if entries
        .len()
        .checked_add(3)
        .is_none_or(|count| count > MAX_OWNER_ENTRIES)
    {
        return Err(invalid_data("Qdrant owner slot inventory is full"));
    }
    for _ in 0..16 {
        let owner = new_owner_id(scope);
        let lifecycle_name = owner_lock_name(owner);
        let (lifecycle, lifecycle_identity) = match create_stable_lock(owners, &lifecycle_name) {
            Ok(created) => created,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        lifecycle.lock_shared()?;
        verify_identity(owners, &lifecycle_name, &lifecycle, lifecycle_identity)?;
        let (_, pin_identity) = create_stable_lock(owners, &owner_pin_lock_name(owner))?;
        return Ok((owner, lifecycle_identity, pin_identity, lifecycle));
    }
    Err(invalid_data(
        "could not allocate a unique Qdrant owner slot",
    ))
}

fn clean_abandoned_temps(
    directory: &DirectoryCapability,
    allowed_directories: &[&str],
    allowed_files: &[&str],
) -> io::Result<()> {
    for entry in directory.entries(MAX_OBJECT_ENTRIES)? {
        let Some(name) = entry.name.to_str() else {
            return Err(invalid_data("non-Unicode Qdrant temp entry"));
        };
        if entry.kind == EntryKind::Directory && allowed_directories.contains(&name) {
            continue;
        }
        if entry.kind == EntryKind::File && allowed_files.contains(&name) {
            continue;
        }
        if entry.kind != EntryKind::File {
            return Err(invalid_data("unexpected Qdrant temp entry kind"));
        }
        if name.ends_with(".tmp") {
            if !name.starts_with("tmp-") {
                return Err(invalid_data("unknown Qdrant temporary file"));
            }
            directory.remove_file(name)?;
        } else if !allowed_files.is_empty() {
            return Err(invalid_data("unknown collection-scoped Qdrant metadata"));
        }
    }
    Ok(())
}

fn verify_identity(
    directory: &DirectoryCapability,
    name: &str,
    file: &File,
    expected: FileIdentity,
) -> io::Result<()> {
    let held = FileIdentity::of_file(file)?;
    let current = directory.open_private_file(name)?;
    let named = FileIdentity::of_file(&current)?;
    if held != expected || named != expected {
        return Err(invalid_data(
            "Qdrant lock name no longer identifies its held inode",
        ));
    }
    Ok(())
}

fn child_or_create(parent: &DirectoryCapability, name: &str) -> io::Result<DirectoryCapability> {
    match parent.open_dir(name) {
        Ok(child) => Ok(child),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match parent.create_private_dir(name) {
                Ok(child) => Ok(child),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => parent.open_dir(name),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn refuse_legacy_flat_state(parent: &DirectoryCapability) -> io::Result<()> {
    for entry in parent.entries(MAX_OBJECT_ENTRIES)? {
        let Some(name) = entry.name.to_str() else {
            return Err(invalid_data("non-Unicode Qdrant state entry"));
        };
        if name.ends_with(".state") || name.ends_with(".tmp") {
            return Err(invalid_data("unsupported legacy Qdrant state preserved"));
        }
    }
    Ok(())
}

fn membership_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher =
        blake3::Hasher::new_derive_key("backend.local-service.qdrant-membership-object.v2");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn object_name(digest: [u8; 32]) -> String {
    format!("{}.obj", hexadecimal(&digest))
}
fn owner_lock_name(owner: [u8; 16]) -> String {
    format!("{}.owner.lock", hexadecimal(&owner))
}
fn owner_pin_lock_name(owner: [u8; 16]) -> String {
    format!("{}.pin.lock", hexadecimal(&owner))
}
fn owner_pin_name(owner: [u8; 16]) -> String {
    format!("{}.pin", hexadecimal(&owner))
}
fn ledger_max_bytes() -> usize {
    8 + 1 + 4 + MAX_DESCRIPTORS * DESCRIPTOR_BYTES + CHECKSUM_BYTES
}

fn new_owner_id(scope: [u8; 32]) -> [u8; 16] {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let mut hasher = blake3::Hasher::new_derive_key("backend.local-service.qdrant-owner.v2");
    hasher.update(&scope);
    hasher.update(&std::process::id().to_be_bytes());
    hasher.update(&sequence.to_be_bytes());
    hasher.update(&nanos.to_be_bytes());
    let hash = hasher.finalize();
    let mut owner = [0_u8; 16];
    owner.copy_from_slice(&hash.as_bytes()[..16]);
    owner
}

fn parse_hex_16(value: &str) -> io::Result<[u8; 16]> {
    if value.len() != 32 {
        return Err(invalid_data("invalid Qdrant owner id"));
    }
    let mut output = [0_u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

fn take32(bytes: &[u8], offset: &mut usize) -> io::Result<[u8; 32]> {
    let end = offset.checked_add(32).ok_or_else(state_limit)?;
    let value = bytes
        .get(*offset..end)
        .ok_or_else(|| invalid_data("short Qdrant descriptor"))?
        .try_into()
        .map_err(|_| invalid_data("short Qdrant descriptor"))?;
    *offset = end;
    Ok(value)
}

fn hex_nibble(byte: u8) -> io::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(invalid_data("noncanonical lowercase hexadecimal")),
    }
}

fn hexadecimal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn state_data() -> io::Error {
    invalid_data("invalid Qdrant state")
}
fn state_limit() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Qdrant state bound exceeded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_encoding_is_canonical_packed_and_roundtrips_auto_sized_sets() {
        let rows = [
            format!("symbol:{}", "f".repeat(64)),
            format!("object:{}", "0".repeat(64)),
            format!("package:{}", "a".repeat(64)),
        ];
        let members = admit_members(&rows).expect("admit rows");
        assert_eq!(
            members.iter().map(|member| member.kind).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        let encoded = encode_membership(&members).expect("encode membership");
        assert_eq!(
            decode_membership(&encoded)
                .expect("decode membership")
                .collect::<Vec<_>>(),
            members
        );
        assert!(
            encoded.len() < 256,
            "membership does not copy row-key strings"
        );
    }

    #[test]
    fn changed_binding_field_changes_residence_prefix_input() {
        let first = qdrant::ProjectionIdentity::from_canonical_fields(
            [1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32],
        );
        let changed = qdrant::ProjectionIdentity::from_canonical_fields(
            [1; 32], [9; 32], [3; 32], [4; 32], [5; 32], [6; 32],
        );
        assert_ne!(first, changed);
    }

    #[test]
    fn missing_collection_ledger_with_a_pin_descriptor_refuses_as_corrupt() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "backend-qdrant-missing-ledger-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("fixture directory");
        let mut state = ProjectionState::open_in_directory(&directory, [0xC3; 32])
            .expect("fresh collection state");
        let owner = state.owner_id;
        let operation = state.begin_operation().expect("operation fence");
        let pin_name = owner_pin_name(owner);
        let mut pin = operation
            .state
            .owners
            .create_file_exclusive(&pin_name)
            .expect("pin fixture");
        pin.write_all(b"preserve this unsupported descriptor")
            .expect("pin fixture write");
        pin.sync_all().expect("pin fixture sync");
        drop(pin);

        assert!(operation.read_ledger().is_err());
        assert!(operation.state.owners.open_file_read(&pin_name).is_ok());
        drop(operation);
        drop(state);
        std::fs::remove_dir_all(directory).expect("remove fixture directory");
    }
}
