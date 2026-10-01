//! Typed GC roots and the durable mark queue item vocabulary.

use super::super::{
    ClosureId, DurableManifest, FileStore, Hash, ObjectEdge, ObjectId, PackId, SelectedHead,
    StoreError,
};
use super::{GC_QUEUE_RECORD_BYTES, MAX_ROOTS};
use crate::UntrustedObjectId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum QueueItem {
    Pack(PackId),
    Closure(ClosureId),
    Object(ObjectId),
    Head {
        pack: PackId,
        closure: ClosureId,
    },
    ManifestPage {
        closure: ClosureId,
        after: ObjectId,
    },
    /// Continue decoding the checked references of one relation object.
    RelationRefs {
        object: ObjectId,
        after: u64,
    },
    /// A selected closure's authenticated membership index. Exact externally
    /// resident member IDs are separate queue roots; every other member is
    /// retained locally.
    RemoteClosureIndex(ClosureId),
    /// A local selected head whose closure has an admitted remote member
    /// allowlist.
    RemoteHead {
        pack: PackId,
        closure: ClosureId,
    },
    /// One closure member whose exact durable remote receipt and product read
    /// fallback were admitted by `GcRootResolver`.
    RemoteMember {
        closure: ClosureId,
        object: ObjectId,
    },
    /// Continue a bounded walk of local closure members not in the remote
    /// residency allowlist.
    RemoteManifestPage {
        closure: ClosureId,
        after: ObjectId,
    },
}

impl QueueItem {
    fn from_root(root: GcRoot) -> Self {
        match root {
            GcRoot::Pack(id) => Self::Pack(id),
            GcRoot::Closure(id) => Self::Closure(id),
            GcRoot::Object(id) => Self::Object(id),
        }
    }

    pub(super) fn encode(self) -> [u8; GC_QUEUE_RECORD_BYTES] {
        let mut bytes = [0; GC_QUEUE_RECORD_BYTES];
        match self {
            Self::Pack(id) => {
                bytes[0] = 1;
                bytes[1..33].copy_from_slice(id.as_bytes());
            }
            Self::Closure(id) => {
                bytes[0] = 2;
                bytes[1..33].copy_from_slice(id.as_bytes());
            }
            Self::Object(id) => {
                bytes[0] = 3;
                bytes[1..33].copy_from_slice(id.as_bytes());
            }
            Self::Head { pack, closure } => {
                bytes[0] = 4;
                bytes[1..33].copy_from_slice(pack.as_bytes());
                bytes[33..65].copy_from_slice(closure.as_bytes());
            }
            Self::ManifestPage { closure, after } => {
                bytes[0] = 5;
                bytes[1..33].copy_from_slice(closure.as_bytes());
                bytes[33..65].copy_from_slice(after.as_bytes());
            }
            Self::RelationRefs { object, after } => {
                bytes[0] = 6;
                bytes[1..33].copy_from_slice(object.as_bytes());
                bytes[33..41].copy_from_slice(&after.to_be_bytes());
            }
            Self::RemoteClosureIndex(id) => {
                bytes[0] = 7;
                bytes[1..33].copy_from_slice(id.as_bytes());
            }
            Self::RemoteHead { pack, closure } => {
                bytes[0] = 8;
                bytes[1..33].copy_from_slice(pack.as_bytes());
                bytes[33..65].copy_from_slice(closure.as_bytes());
            }
            Self::RemoteMember { closure, object } => {
                bytes[0] = 9;
                bytes[1..33].copy_from_slice(closure.as_bytes());
                bytes[33..65].copy_from_slice(object.as_bytes());
            }
            Self::RemoteManifestPage { closure, after } => {
                bytes[0] = 10;
                bytes[1..33].copy_from_slice(closure.as_bytes());
                bytes[33..65].copy_from_slice(after.as_bytes());
            }
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8; GC_QUEUE_RECORD_BYTES]) -> Result<Self, StoreError> {
        let first: Hash = bytes[1..33].try_into().map_err(|_| StoreError::Corrupt)?;
        let second: Hash = bytes[33..65].try_into().map_err(|_| StoreError::Corrupt)?;
        match bytes[0] {
            1 => Ok(Self::Pack(PackId::from_wire(first))),
            2 => Ok(Self::Closure(ClosureId::from_bytes(first))),
            3 => Ok(Self::Object(ObjectId::from_bytes(first))),
            4 => Ok(Self::Head {
                pack: PackId::from_wire(first),
                closure: ClosureId::from_bytes(second),
            }),
            5 => Ok(Self::ManifestPage {
                closure: ClosureId::from_bytes(first),
                after: ObjectId::from_bytes(second),
            }),
            6 if bytes[41..].iter().all(|byte| *byte == 0) => Ok(Self::RelationRefs {
                object: ObjectId::from_bytes(first),
                after: u64::from_be_bytes(
                    bytes[33..41].try_into().map_err(|_| StoreError::Corrupt)?,
                ),
            }),
            7 if second == [0; 32] => Ok(Self::RemoteClosureIndex(ClosureId::from_bytes(first))),
            8 => Ok(Self::RemoteHead {
                pack: PackId::from_wire(first),
                closure: ClosureId::from_bytes(second),
            }),
            9 => Ok(Self::RemoteMember {
                closure: ClosureId::from_bytes(first),
                object: ObjectId::from_bytes(second),
            }),
            10 => Ok(Self::RemoteManifestPage {
                closure: ClosureId::from_bytes(first),
                after: ObjectId::from_bytes(second),
            }),
            _ => Err(StoreError::Corrupt),
        }
    }
}

/// A physical immutable root accepted by the collector.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GcRoot {
    /// One immutable map pack or tree-pack descriptor.
    Pack(PackId),
    /// One complete closure manifest.
    Closure(ClosureId),
    /// One typed immutable object.
    Object(ObjectId),
}

/// Typed roots retained by readers, pins, leases, and product catalogs.
///
/// Callers add a selected [`SelectedHead`] with
/// [`Self::add_selected_head`].  The head binds its pack and closure as one
/// root record, so a tree publication can retain its root relation node before
/// the complete node walk runs.  Reader, transfer, checkpoint, and derived
/// output helpers intentionally accept the same [`GcRoot`] vocabulary; the
/// collector therefore does not parse those product records.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GcRoots {
    items: Vec<QueueItem>,
    overflowed: bool,
}

impl GcRoots {
    /// Creates an empty root set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            items: Vec::new(),
            overflowed: false,
        }
    }

    /// Creates roots containing one selected durable head.
    #[must_use]
    pub fn selected_head(head: SelectedHead) -> Self {
        let mut roots = Self::new();
        roots.add_selected_head(head);
        roots
    }

    /// Adds the selected pack and its closure as one authenticated head root.
    pub fn add_selected_head(&mut self, head: SelectedHead) {
        self.push(QueueItem::Head {
            pack: head.descriptor().pack(),
            closure: head.descriptor().closure(),
        });
    }

    /// Adds an arbitrary immutable root.
    pub fn add(&mut self, root: GcRoot) {
        self.push(QueueItem::from_root(root));
    }

    /// Adds every root from another checked snapshot.
    pub fn extend(&mut self, other: Self) {
        self.overflowed |= other.overflowed;
        for item in other.items {
            self.push(item);
        }
    }

    /// Adds a reader or pin root.
    pub fn add_reader(&mut self, root: GcRoot) {
        self.add(root);
    }

    /// Adds a transfer lease root.
    pub fn add_transfer_lease(&mut self, root: GcRoot) {
        self.add(root);
    }

    /// Adds a checkpoint root.
    pub fn add_checkpoint(&mut self, root: GcRoot) {
        self.add(root);
    }

    /// Adds a derived-output catalog root.
    pub fn add_derived_output(&mut self, root: GcRoot) {
        self.add(root);
    }

    /// Adds a root retained by a replication lease.
    pub fn add_replication_lease(&mut self, root: GcRoot) {
        self.add_transfer_lease(root);
    }

    /// Adds explicit immutable-object edges supplied by a checked catalog.
    ///
    /// Both endpoints are retained as roots for this collection.  This is a
    /// conservative boundary for catalogs whose source object may be in a
    /// different retained closure; it never guesses edges by parsing a
    /// product payload.
    pub fn add_edges<I>(&mut self, edges: I)
    where
        I: IntoIterator<Item = ObjectEdge>,
    {
        for edge in edges {
            self.add(GcRoot::Object(edge.from()));
            self.add(GcRoot::Object(edge.to()));
        }
    }

    pub(super) fn normalized_items(&self, limit: usize) -> Result<Vec<QueueItem>, StoreError> {
        if self.overflowed {
            return Err(StoreError::Bounds);
        }
        let mut items = self.items.clone();
        items.sort_unstable();
        items.dedup();
        if items.len() > limit || items.len() > MAX_ROOTS {
            return Err(StoreError::Bounds);
        }
        Ok(items)
    }

    pub(super) fn has_remote_closure_index(&self, closure: ClosureId) -> bool {
        self.items.contains(&QueueItem::RemoteClosureIndex(closure))
    }

    pub(super) fn has_remote_closure_indexes(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                QueueItem::RemoteClosureIndex(_) | QueueItem::RemoteMember { .. }
            )
        })
    }

    pub(super) fn has_full_closure_root(&self, closure: ClosureId) -> bool {
        self.items.iter().any(|item| match item {
            QueueItem::Closure(id) | QueueItem::ManifestPage { closure: id, .. } => *id == closure,
            QueueItem::Head { closure: id, .. } => *id == closure,
            _ => false,
        })
    }

    pub(super) fn add_store_selected_head(&mut self, head: SelectedHead) {
        let pack = head.descriptor().pack();
        let closure = head.descriptor().closure();
        if self.has_remote_closure_index(closure) && !self.has_full_closure_root(closure) {
            self.push(QueueItem::RemoteHead { pack, closure });
        } else {
            self.add_selected_head(head);
        }
    }

    fn push(&mut self, item: QueueItem) {
        if self.items.len() < MAX_ROOTS {
            self.items.push(item);
        } else {
            self.overflowed = true;
        }
    }
}

/// Root collector scoped to the exclusive GC lease.
///
/// Remote residency is deliberately absent from [`GcRoot`]. It can only be
/// established while [`FileStore::collect_garbage_resolving_roots`] owns the
/// store's exclusive collector lease, and only through
/// [`Self::add_remote_closure_members`] with an owner-supplied verifier for
/// the current selected authority, exact durable receipt, and every listed
/// member's production fallback. Unlisted members remain locally rooted.
pub struct GcRootResolver<'a> {
    store: &'a FileStore,
    roots: GcRoots,
}

impl<'a> GcRootResolver<'a> {
    pub(super) const fn new(store: &'a FileStore) -> Self {
        Self {
            store,
            roots: GcRoots::new(),
        }
    }

    /// Adds an ordinary local root.
    pub fn add(&mut self, root: GcRoot) {
        self.roots.add(root);
    }

    /// Adds a remote residency allowlist after checking the exact closure
    /// membership locally and asking the index owner to verify current
    /// authority, its durable remote receipt, and a complete production read
    /// fallback for each listed member.
    ///
    /// The verifier runs while the store's exclusive GC lease is held. It is
    /// the intentional trust boundary for remote durability and product
    /// fallback; returning success authorizes GC to reclaim only the listed
    /// member envelopes. Every other member is streamed from the authenticated
    /// local closure index into the mark queue and stays local. The number of
    /// listed members plus existing roots is bounded by `MAX_ROOTS`.
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if a listed ID is not in the checked
    /// closure, if the verifier rejects its receipt/fallback, or if the list
    /// contains a duplicate. Returns [`StoreError::Bounds`] if the allowlist
    /// exceeds the durable root limit.
    pub fn add_remote_closure_members<I, F>(
        &mut self,
        closure: ClosureId,
        remote_members: I,
        verify_remote: F,
    ) -> Result<(), StoreError>
    where
        I: IntoIterator<Item = ObjectId>,
        F: FnOnce(ClosureId, &[ObjectId]) -> Result<(), StoreError>,
    {
        let manifest = self.store.open_closure(closure)?;
        self.add_remote_closure_members_inner(
            closure,
            &manifest,
            remote_members,
            verify_remote,
            |checked, member| {
                if checked.contains_object_id(member)? {
                    Ok(member)
                } else {
                    Err(StoreError::Corrupt)
                }
            },
        )
    }

    /// Adds remote member claims received as untrusted digest bytes. Each
    /// claim is promoted to `ObjectId` only after the authenticated closure
    /// index admits it as an exact member; possession of raw ID bytes alone
    /// never authorizes GC reachability or eviction.
    pub fn add_remote_closure_member_claims<I, F>(
        &mut self,
        closure: ClosureId,
        remote_members: I,
        verify_remote: F,
    ) -> Result<(), StoreError>
    where
        I: IntoIterator<Item = UntrustedObjectId>,
        F: FnOnce(ClosureId, &[ObjectId]) -> Result<(), StoreError>,
    {
        let manifest = self.store.open_closure(closure)?;
        self.add_remote_closure_members_inner(
            closure,
            &manifest,
            remote_members,
            verify_remote,
            |checked, claim| checked.admit_claim(claim)?.ok_or(StoreError::Corrupt),
        )
    }

    fn add_remote_closure_members_inner<I, T, F, A>(
        &mut self,
        closure: ClosureId,
        manifest: &DurableManifest,
        remote_members: I,
        verify_remote: F,
        mut admit_member: A,
    ) -> Result<(), StoreError>
    where
        I: IntoIterator<Item = T>,
        F: FnOnce(ClosureId, &[ObjectId]) -> Result<(), StoreError>,
        A: FnMut(&DurableManifest, T) -> Result<ObjectId, StoreError>,
    {
        if self.roots.items.len().saturating_add(1) > MAX_ROOTS {
            return Err(StoreError::Bounds);
        }
        let mut members = Vec::new();
        for claim in remote_members {
            if members
                .len()
                .saturating_add(self.roots.items.len())
                .saturating_add(1)
                >= MAX_ROOTS
            {
                return Err(StoreError::Bounds);
            }
            members.push(admit_member(manifest, claim)?);
        }
        members.sort_unstable();
        if members.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(StoreError::Corrupt);
        }
        verify_remote(closure, &members)?;
        self.roots.push(QueueItem::RemoteClosureIndex(closure));
        for object in members {
            self.roots.push(QueueItem::RemoteMember { closure, object });
        }
        Ok(())
    }

    pub(super) fn into_roots(self) -> GcRoots {
        self.roots
    }
}
