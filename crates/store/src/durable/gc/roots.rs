//! Typed GC roots and the durable mark queue item vocabulary.

use super::super::{ClosureId, Hash, ObjectEdge, ObjectId, PackId, SelectedHead, StoreError};
use super::{GC_QUEUE_RECORD_BYTES, MAX_ROOTS};

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

    fn push(&mut self, item: QueueItem) {
        if self.items.len() < MAX_ROOTS {
            self.items.push(item);
        } else {
            self.overflowed = true;
        }
    }
}
