//! Collection-scoped, crash-safe ownership and membership for Qdrant.
//!
//! One operation lock fences staging, remote publication, pin replacement,
//! retirement, and commit. Membership bytes are immutable content-addressed
//! objects; pin and ledger records contain only bounded descriptors.

use backend_extension_qdrant as qdrant;
use backend_platform::{DirectoryCapability, EntryKind, FileIdentity};
use backend_version::WorkspaceRoot;
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
}

/// Exact durable descriptor for one full Qdrant binding and immutable member
/// set. `fields` are workspace, root, recipe, authority, read manifest, and
/// frontier in that order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Descriptor {
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
    _lock: File,
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
            io::Error::new(io::ErrorKind::NotFound, "Qdrant durable data directory is unset")
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
    pub(super) fn open_in_directory(
        parent: &Path,
        scope: [u8; 32],
    ) -> io::Result<Self> {
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
        let owner_id = new_owner_id(scope);
        let owner_lock_name = owner_lock_name(owner_id);
        let owner_lock_identity = open_stable_lock(&owners, &owner_lock_name)?;
        Ok(Self {
            scope,
            directory,
            owners,
            objects,
            operation_lock_identity,
            owner_id,
            owner_lock_identity,
        })
    }

    /// Begins the sole mutation/publication transaction for this collection.
    pub(super) fn begin_operation(&mut self) -> io::Result<ProjectionOperation<'_>> {
        let lock = self
            .directory
            .open_private_file_read_write(OPERATION_LOCK, false)?;
        verify_identity(&self.directory, OPERATION_LOCK, &lock, self.operation_lock_identity)?;
        lock.lock()?;
        verify_identity(&self.directory, OPERATION_LOCK, &lock, self.operation_lock_identity)?;
        Ok(ProjectionOperation { state: self, _lock: lock })
    }
}

impl ProjectionOperation<'_> {
    /// Persists the immutable target object and the union of prior debt, live
    /// owner pins, and this target before any remote upsert can be issued.
    pub(super) fn stage(
        &mut self,
        binding: qdrant::Binding,
        row_keys: &[String],
    ) -> io::Result<Descriptor> {
        let target = self.state.persist_membership(binding, row_keys)?;
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
        if descriptor.scope != self.state.scope {
            return Err(invalid_data("Qdrant pin belongs to another provider"));
        }
        let lock_name = owner_lock_name(self.state.owner_id);
        let pin_lock = self
            .state
            .owners
            .open_private_file_read_write(&lock_name, false)?;
        verify_identity(
            &self.state.owners,
            &lock_name,
            &pin_lock,
            self.state.owner_lock_identity,
        )?;
        pin_lock.lock_shared()?;
        verify_identity(
            &self.state.owners,
            &lock_name,
            &pin_lock,
            self.state.owner_lock_identity,
        )?;
        let encoded = encode_pin(self.state.scope, self.state.owner_id, descriptor);
        atomic_replace(
            &self.state.owners,
            &owner_pin_name(self.state.owner_id),
            &encoded,
        )?;
        Ok(ProjectionPin {
            descriptor,
            _lock: pin_lock,
        })
    }

    /// Computes stale point residences for the configured recipe while
    /// excluding the target and every currently live pin across workspaces.
    pub(super) fn stale_residences(
        &mut self,
        target: Descriptor,
    ) -> io::Result<Vec<(qdrant::ProjectionIdentity, Vec<qdrant::PointResidence>)>> {
        let descriptors = self.read_ledger()?;
        let pins = self.live_pin_descriptors()?;
        let mut protected = HashSet::<(qdrant::ProjectionIdentity, Member)>::new();
        for descriptor in pins.iter().copied().chain(std::iter::once(target)) {
            for member in self.read_members(descriptor)? {
                protected.insert((descriptor.identity, member));
            }
        }
        let target_recipe = target.fields[2];
        let mut stale = BTreeMap::<qdrant::ProjectionIdentity, BTreeSet<qdrant::PointResidence>>::new();
        for descriptor in descriptors {
            if descriptor.fields[2] != target_recipe {
                continue;
            }
            for member in self.read_members(descriptor)? {
                if protected.contains(&(descriptor.identity, member)) {
                    continue;
                }
                let key = member.stable_key();
                let residence = qdrant::PointResidence::for_row(descriptor.identity, key)
                    .map_err(|_| invalid_data("invalid Qdrant membership key"))?;
                stale.entry(descriptor.identity).or_default().insert(residence);
            }
        }
        Ok(stale
            .into_iter()
            .map(|(identity, residences)| (identity, residences.into_iter().collect()))
            .collect())
    }

    /// Commits only the exact target, all live pins, and foreign-recipe debt
    /// that this configured client cannot retire.
    pub(super) fn commit_verified(&mut self, target: Descriptor) -> io::Result<()> {
        let mut retained = self.live_pin_descriptors()?;
        retained.push(target);
        for descriptor in self.read_ledger()? {
            if descriptor.fields[2] != target.fields[2] {
                retained.push(descriptor);
            }
        }
        let retained = canonical_descriptors(retained, self.state.scope)?;
        self.write_ledger(&retained)?;
        self.reclaim_unreferenced_objects(&retained)?;
        Ok(())
    }

    fn read_ledger(&self) -> io::Result<Vec<Descriptor>> {
        let bytes = read_bounded(&self.state.directory, "collection.ledger", ledger_max_bytes())?;
        match bytes {
            None => Ok(Vec::new()),
            Some(bytes) => decode_ledger(&bytes, self.state.scope),
        }
    }

    fn write_ledger(&self, descriptors: &[Descriptor]) -> io::Result<()> {
        let bytes = encode_ledger(descriptors, self.state.scope)?;
        atomic_replace(&self.state.directory, "collection.ledger", &bytes)
    }

    fn live_pin_descriptors(&self) -> io::Result<Vec<Descriptor>> {
        let entries = self.state.owners.entries(MAX_OWNER_ENTRIES)?;
        let mut descriptors = Vec::new();
        let mut names = HashSet::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                return Err(invalid_data("unexpected Qdrant owner entry kind"));
            }
            let Some(name) = entry.name.to_str() else {
                return Err(invalid_data("non-Unicode Qdrant owner entry"));
            };
            let Some(owner_hex) = name.strip_suffix(".lock") else {
                if name.ends_with(".tmp") {
                    // The collection fence proves only an interrupted atomic
                    // descriptor write can leave this recognized temp behind.
                    self.state.owners.remove_file(name)?;
                    continue;
                }
                if name.ends_with(".pin") {
                    continue;
                }
                return Err(invalid_data("unknown Qdrant owner metadata"));
            };
            let owner = parse_hex_16(owner_hex)?;
            names.insert(owner);
            let lock_name = owner_lock_name(owner);
            let lock = self.state.owners.open_private_file_read_write(&lock_name, false)?;
            let identity = FileIdentity::of_file(&lock)?;
            verify_identity(&self.state.owners, &lock_name, &lock, identity)?;
            match lock.try_lock() {
                Ok(()) => {
                    // Exclusive acquisition proves the owner is not live. Its
                    // descriptor can be removed, but the stable lock inode is
                    // deliberately retained forever.
                    match self.state.owners.remove_file(&owner_pin_name(owner)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let pin = read_bounded(
                        &self.state.owners,
                        &owner_pin_name(owner),
                        PIN_BYTES,
                    )?
                    .ok_or_else(|| invalid_data("live Qdrant owner has no pin descriptor"))?;
                    let descriptor = decode_pin(&pin, self.state.scope, owner)?;
                    descriptors.push(descriptor);
                    let _ = File::unlock(&lock);
                }
                Err(error) => return Err(error),
            }
        }
        if names.len() > MAX_OWNER_ENTRIES / 2 || descriptors.len() > MAX_DESCRIPTORS {
            return Err(invalid_data("Qdrant live pin inventory limit exceeded"));
        }
        Ok(descriptors)
    }

    fn read_members(&self, descriptor: Descriptor) -> io::Result<Vec<Member>> {
        let name = object_name(descriptor.membership);
        let bytes = read_bounded(&self.state.objects, &name, MAX_MEMBERSHIP_BYTES)?
            .ok_or_else(|| invalid_data("Qdrant membership object is missing"))?;
        let expected_hash = membership_hash(&bytes);
        if expected_hash != descriptor.membership {
            return Err(invalid_data("Qdrant membership object digest mismatch"));
        }
        let members = decode_membership(&bytes)?;
        if members.len() != usize::try_from(descriptor.count).unwrap_or(usize::MAX) {
            return Err(invalid_data("Qdrant membership count mismatch"));
        }
        Ok(members)
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
                decode_membership(&bytes)?;
                self.state.objects.remove_file(name)?;
            }
        }
        Ok(())
    }
}

impl ProjectionState {
    fn persist_membership(
        &self,
        binding: qdrant::Binding,
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
                atomic_create(&self.objects, &name, &bytes)?;
            }
            Err(error) => return Err(error),
        }
        let fields = binding_fields(binding);
        let identity = qdrant::ProjectionIdentity::from_canonical_fields(
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        );
        Ok(Descriptor {
            scope: self.scope,
            fields,
            identity,
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

fn residence_key(member: Member, scratch: &mut [u8; 71]) -> &str {
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
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
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
    output.try_reserve_exact(capacity).map_err(|_| io::Error::other("Qdrant membership allocation failed"))?;
    output.extend_from_slice(MEMBERSHIP_MAGIC);
    output.push(VERSION);
    output.extend_from_slice(&u32::try_from(members.len()).map_err(|_| state_limit())?.to_be_bytes());
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
    append_checksum(b"backend.local-service.qdrant-membership.v2", &mut output);
    Ok(output)
}

fn decode_membership(bytes: &[u8]) -> io::Result<Vec<Member>> {
    if bytes.len() < 8 + 1 + 4 + CHECKSUM_BYTES || bytes.len() > MAX_MEMBERSHIP_BYTES || &bytes[..8] != MEMBERSHIP_MAGIC || bytes[8] != VERSION {
        return Err(invalid_data("invalid Qdrant membership header"));
    }
    check_checksum(b"backend.local-service.qdrant-membership.v2", bytes)?;
    let count = usize::try_from(u32::from_be_bytes(bytes[9..13].try_into().map_err(|_| invalid_data("invalid member count"))?)).map_err(|_| state_limit())?;
    if count > MAX_ROWS {
        return Err(invalid_data("Qdrant membership count limit exceeded"));
    }
    let kinds_len = count.checked_add(3).ok_or_else(state_limit)? / 4;
    let lane_start = 13_usize.checked_add(kinds_len).ok_or_else(state_limit)?;
    let checksum_at = bytes.len() - CHECKSUM_BYTES;
    let expected = lane_start.checked_add(count.checked_mul(32).ok_or_else(state_limit)?).ok_or_else(state_limit)?;
    if expected != checksum_at {
        return Err(invalid_data("invalid Qdrant membership size"));
    }
    if count % 4 != 0 && kinds_len > 0 {
        let used = (count % 4) * 2;
        if bytes[12 + kinds_len] & !((1_u8 << used) - 1) != 0 {
            return Err(invalid_data("nonzero Qdrant membership padding"));
        }
    }
    let mut members = Vec::new();
    members.try_reserve_exact(count).map_err(|_| io::Error::other("Qdrant membership allocation failed"))?;
    for index in 0..count {
        let kind = (bytes[13 + index / 4] >> ((index % 4) * 2)) & 0b11;
        if kind > 2 {
            return Err(invalid_data("invalid packed Qdrant row kind"));
        }
        let offset = lane_start + index * 32;
        let digest = bytes[offset..offset + 32].try_into().map_err(|_| invalid_data("invalid Qdrant digest lane"))?;
        let member = Member { kind, digest };
        if members.last().is_some_and(|prior| prior >= &member) {
            return Err(invalid_data("unordered Qdrant membership"));
        }
        members.push(member);
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

fn canonical_descriptors(mut descriptors: Vec<Descriptor>, scope: [u8; 32]) -> io::Result<Vec<Descriptor>> {
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid_data("Qdrant descriptor limit exceeded"));
    }
    for descriptor in &descriptors {
        let identity = qdrant::ProjectionIdentity::from_canonical_fields(
            descriptor.fields[0], descriptor.fields[1], descriptor.fields[2],
            descriptor.fields[3], descriptor.fields[4], descriptor.fields[5],
        );
        if descriptor.scope != scope || descriptor.identity != identity {
            return Err(invalid_data("Qdrant descriptor binding mismatch"));
        }
    }
    descriptors.sort_by_key(|descriptor| (descriptor.identity, descriptor.membership));
    descriptors.dedup();
    for pair in descriptors.windows(2) {
        if pair[0].identity == pair[1].identity && pair[0].membership != pair[1].membership {
            return Err(invalid_data("one Qdrant binding has conflicting membership"));
        }
    }
    Ok(descriptors)
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
    let count = u32::from_be_bytes(bytes[offset..offset + 4].try_into().map_err(|_| invalid_data("invalid Qdrant descriptor count"))?);
    if usize::try_from(count).unwrap_or(usize::MAX) > MAX_ROWS {
        return Err(invalid_data("Qdrant descriptor count limit exceeded"));
    }
    let identity = qdrant::ProjectionIdentity::from_canonical_fields(fields[0], fields[1], fields[2], fields[3], fields[4], fields[5]);
    if identity.as_bytes() != &stored_identity {
        return Err(invalid_data("Qdrant descriptor identity mismatch"));
    }
    Ok(Descriptor { scope, fields, identity, membership, count })
}

fn encode_ledger(descriptors: &[Descriptor], scope: [u8; 32]) -> io::Result<Vec<u8>> {
    let descriptors = canonical_descriptors(descriptors.to_vec(), scope)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(8 + 1 + 4 + descriptors.len() * DESCRIPTOR_BYTES + CHECKSUM_BYTES)
        .map_err(|_| io::Error::other("Qdrant ledger allocation failed"))?;
    bytes.extend_from_slice(LEDGER_MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&u32::try_from(descriptors.len()).map_err(|_| state_limit())?.to_be_bytes());
    for descriptor in descriptors {
        encode_descriptor(descriptor, &mut bytes);
    }
    append_checksum(b"backend.local-service.qdrant-ledger.v2", &mut bytes);
    Ok(bytes)
}

fn decode_ledger(bytes: &[u8], scope: [u8; 32]) -> io::Result<Vec<Descriptor>> {
    if bytes.len() < 8 + 1 + 4 + CHECKSUM_BYTES || &bytes[..8] != LEDGER_MAGIC || bytes[8] != VERSION {
        return Err(invalid_data("invalid Qdrant ledger header"));
    }
    check_checksum(b"backend.local-service.qdrant-ledger.v2", bytes)?;
    let count = usize::try_from(u32::from_be_bytes(bytes[9..13].try_into().map_err(|_| invalid_data("invalid Qdrant ledger count"))?)).map_err(|_| state_limit())?;
    if count > MAX_DESCRIPTORS || 13 + count * DESCRIPTOR_BYTES + CHECKSUM_BYTES != bytes.len() {
        return Err(invalid_data("invalid Qdrant ledger size"));
    }
    let mut descriptors = Vec::new();
    descriptors.try_reserve_exact(count).map_err(|_| io::Error::other("Qdrant ledger allocation failed"))?;
    let mut offset = 13;
    for _ in 0..count {
        descriptors.push(decode_descriptor(&bytes[offset..offset + DESCRIPTOR_BYTES])?);
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
    append_checksum(b"backend.local-service.qdrant-pin.v2", &mut bytes);
    debug_assert_eq!(bytes.len(), PIN_BYTES);
    debug_assert_eq!(descriptor.scope, scope);
    bytes
}

fn decode_pin(bytes: &[u8], scope: [u8; 32], owner: [u8; 16]) -> io::Result<Descriptor> {
    if bytes.len() != PIN_BYTES || &bytes[..8] != PIN_MAGIC || bytes[8] != VERSION || bytes[9..25] != owner {
        return Err(invalid_data("invalid Qdrant pin header"));
    }
    check_checksum(b"backend.local-service.qdrant-pin.v2", bytes)?;
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
    let checksum_at = bytes.len().checked_sub(CHECKSUM_BYTES).ok_or_else(state_data)?;
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(&bytes[..checksum_at]);
    if bytes[checksum_at..] != *hasher.finalize().as_bytes() {
        return Err(invalid_data("Qdrant state checksum mismatch"));
    }
    Ok(())
}

fn read_bounded(directory: &DirectoryCapability, name: &str, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut file = match directory.open_private_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(invalid_data("Qdrant state file exceeds its bound"));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(metadata.len()).map_err(|_| state_limit())?)
        .map_err(|_| io::Error::other("Qdrant state allocation failed"))?;
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid_data("Qdrant state file exceeds its bound"));
    }
    Ok(Some(bytes))
}

fn atomic_replace(directory: &DirectoryCapability, destination: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_name(destination);
    let mut file = directory.create_file_exclusive(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = directory.remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    directory
        .rename_with_outcome(&temporary, destination, true)
        .map_err(|error| error.into_io_error())
}

fn atomic_create(directory: &DirectoryCapability, destination: &str, bytes: &[u8]) -> io::Result<()> {
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
    format!("tmp-{}-{sequence}-{}", std::process::id(), &blake3::hash(destination.as_bytes()).to_hex()[..12])
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

fn verify_identity(directory: &DirectoryCapability, name: &str, file: &File, expected: FileIdentity) -> io::Result<()> {
    let held = FileIdentity::of_file(file)?;
    let current = directory.open_private_file(name)?;
    let named = FileIdentity::of_file(&current)?;
    if held != expected || named != expected {
        return Err(invalid_data("Qdrant lock name no longer identifies its held inode"));
    }
    Ok(())
}

fn child_or_create(parent: &DirectoryCapability, name: &str) -> io::Result<DirectoryCapability> {
    match parent.open_dir(name) {
        Ok(child) => Ok(child),
        Err(error) if error.kind() == io::ErrorKind::NotFound => parent.create_private_dir(name),
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
    let mut hasher = blake3::Hasher::new_derive_key("backend.local-service.qdrant-membership-object.v2");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn object_name(digest: [u8; 32]) -> String { format!("{}.obj", hexadecimal(&digest)) }
fn owner_lock_name(owner: [u8; 16]) -> String { format!("{}.lock", hexadecimal(&owner)) }
fn owner_pin_name(owner: [u8; 16]) -> String { format!("{}.pin", hexadecimal(&owner)) }
fn ledger_max_bytes() -> usize { 8 + 1 + 4 + MAX_DESCRIPTORS * DESCRIPTOR_BYTES + CHECKSUM_BYTES }

fn new_owner_id(scope: [u8; 32]) -> [u8; 16] {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos());
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
    if value.len() != 32 { return Err(invalid_data("invalid Qdrant owner id")); }
    let mut output = [0_u8; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

fn take32(bytes: &[u8], offset: &mut usize) -> io::Result<[u8; 32]> {
    let end = offset.checked_add(32).ok_or_else(state_limit)?;
    let value = bytes.get(*offset..end).ok_or_else(|| invalid_data("short Qdrant descriptor"))?.try_into().map_err(|_| invalid_data("short Qdrant descriptor"))?;
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

fn invalid_data(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
fn state_data() -> io::Error { invalid_data("invalid Qdrant state") }
fn state_limit() -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, "Qdrant state bound exceeded") }

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
        assert_eq!(members.iter().map(|member| member.kind).collect::<Vec<_>>(), [0, 1, 2]);
        let encoded = encode_membership(&members).expect("encode membership");
        assert_eq!(decode_membership(&encoded).expect("decode membership"), members);
        assert!(encoded.len() < 256, "membership does not copy row-key strings");
    }

    #[test]
    fn changed_binding_field_changes_residence_prefix_input() {
        let first = qdrant::ProjectionIdentity::from_canonical_fields([1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32]);
        let changed = qdrant::ProjectionIdentity::from_canonical_fields([1; 32], [9; 32], [3; 32], [4; 32], [5; 32], [6; 32]);
        assert_ne!(first, changed);
    }
}
