//! Owner-local, ephemeral comparison material. This module stores only bytes
//! the native owner has already admitted; it cannot open a historical root or
//! read a current file. It is not a public route or a source authority.
use std::mem::size_of;
use std::time::{Duration, Instant};

const MAX_LEASES: usize = 4;
const MAX_LEASE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WORKSPACE_BYTES: usize = 128 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(600);

/// Attempt status observed at capture time; codes 0..8 identify the native
/// unavailable reason and are decoded only by the typed owner adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Freshness {
    Unknown,
    UnknownLegacy,
    LatestObserved,
    RetainedAfterUnavailable(u8),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Worker {
    Unknown,
    NoPermit,
    Admitted([u8; 32]),
}
/// Identity of the exact selected owner, source and producer inputs. Recipe
/// and scope are opaque owner-computed digests, not filenames or OIDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Basis {
    pub manifest: [u8; 32],
    pub native: [u8; 32],
    pub project: [u8; 32],
    pub scope: [u8; 32],
    pub recipe: [u8; 32],
    pub freshness: Freshness,
    pub worker: Worker,
}
/// A bounded inventory cannot assert absence when any portion was missed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Coverage {
    Complete,
    Gapped,
}
/// Exact relation row or verified captured source image, keyed by its owner.
#[derive(Debug)]
pub(super) struct Material {
    pub key: [u8; 32],
    pub bytes: Box<[u8]>,
}
/// Owner-local handle. Epoch changes on restart; it is not authentication for
/// any public boundary, which must additionally use the native owner MAC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LeaseId {
    epoch: [u8; 16],
    sequence: u64,
}
impl LeaseId {
    pub fn parts(self) -> ([u8; 16], u64) {
        (self.epoch, self.sequence)
    }
    pub fn from_parts(epoch: [u8; 16], sequence: u64) -> Self {
        Self { epoch, sequence }
    }
}
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Error {
    Expired,
    Capacity,
    Cancelled,
    Invalid,
    Exhausted,
}

/// Only constructed by the admitted capture adapter. `records` are canonical
/// relation rows and `sources` are identity-verified bytes from that basis.
/// Missing source bytes must set source coverage to Gapped, never be read later.
pub(super) struct Capture {
    pub basis: Basis,
    pub relation_coverage: Coverage,
    pub source_coverage: Coverage,
    pub records: Vec<Material>,
    pub sources: Vec<Material>,
}
pub(super) struct Lease {
    pub basis: Basis,
    pub relation_coverage: Coverage,
    pub source_coverage: Coverage,
    pub records: Box<[Material]>,
    pub sources: Box<[Material]>,
    bytes: usize,
    expires: Instant,
    id: LeaseId,
}
impl Lease {
    /// Only captured, identity-verified bytes are returned. This does not open
    /// the host filesystem when an image was omitted or is no longer retained.
    pub fn historical_source(&self, host: &[u8; 32]) -> Option<&[u8]> {
        self.sources
            .binary_search_by_key(host, |item| item.key)
            .ok()
            .map(|index| self.sources[index].bytes.as_ref())
    }
}
/// Explicit comparison side. A caller must separately prove change events;
/// removed seeds belong to Base and added seeds belong to Target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChangeSide {
    Base,
    Target,
}
/// One workspace owner's volatile lease table; no eviction fabricates absence.
pub(super) struct LeaseStore {
    epoch: [u8; 16],
    next: u64,
    used: usize,
    slots: [Option<Lease>; MAX_LEASES],
}
impl LeaseStore {
    /// Unpredictable per-process epoch; failure leaves historical retention
    /// unavailable rather than reusing an epoch or weakening current reads.
    pub fn new_random() -> Option<Self> {
        let mut epoch = [0; 16];
        fill_epoch(&mut epoch).ok()?;
        Some(Self::new(epoch))
    }
    pub fn new(epoch: [u8; 16]) -> Self {
        Self {
            epoch,
            next: 0,
            used: size_of::<Self>(),
            slots: std::array::from_fn(|_| None),
        }
    }
    fn expire(&mut self, now: Instant) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|lease| now >= lease.expires) {
                if let Some(lease) = slot.take() {
                    self.used -= lease.bytes;
                }
            }
        }
    }
    /// Retain already validated bytes. Checks bounds and cancellation before
    /// publication; failures leave existing leases and accounting unchanged.
    pub fn insert<F: FnMut() -> bool>(
        &mut self,
        mut capture: Capture,
        now: Instant,
        mut cancelled: F,
    ) -> Result<LeaseId, Error> {
        self.expire(now);
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let count = capture
            .records
            .len()
            .checked_add(capture.sources.len())
            .ok_or(Error::Capacity)?;
        if count > MAX_LEASE_BYTES / size_of::<Material>() {
            return Err(Error::Capacity);
        }
        let mut bytes = size_of::<Lease>()
            .checked_add(
                count
                    .checked_mul(size_of::<Material>())
                    .ok_or(Error::Capacity)?,
            )
            .ok_or(Error::Capacity)?;
        for item in capture.records.iter().chain(&capture.sources) {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            bytes = bytes.checked_add(item.bytes.len()).ok_or(Error::Capacity)?;
            if bytes > MAX_LEASE_BYTES {
                return Err(Error::Capacity);
            }
        }
        if self
            .used
            .checked_add(bytes)
            .is_none_or(|v| v > MAX_WORKSPACE_BYTES)
        {
            return Err(Error::Capacity);
        }
        // Cap work before sorting and reject duplicate keys within each namespace.
        capture.records.sort_unstable_by_key(|item| item.key);
        capture.sources.sort_unstable_by_key(|item| item.key);
        if capture.records.windows(2).any(|w| w[0].key == w[1].key)
            || capture.sources.windows(2).any(|w| w[0].key == w[1].key)
        {
            return Err(Error::Invalid);
        }
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let expires = now.checked_add(TTL).ok_or(Error::Exhausted)?;
        let sequence = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let id = LeaseId {
            epoch: self.epoch,
            sequence,
        };
        let lease = Lease {
            basis: capture.basis,
            relation_coverage: capture.relation_coverage,
            source_coverage: capture.source_coverage,
            records: capture.records.into_boxed_slice(),
            sources: capture.sources.into_boxed_slice(),
            bytes,
            expires,
            id,
        };
        self.slots[slot] = Some(lease);
        self.next = sequence;
        self.used += bytes;
        Ok(id)
    }
    /// Missing, expired, and previous-owner handles are never resolved from disk.
    pub fn get(&mut self, id: LeaseId, now: Instant) -> Result<&Lease, Error> {
        self.expire(now);
        if id.epoch != self.epoch {
            return Err(Error::Expired);
        }
        self.slots
            .iter()
            .flatten()
            .find(|lease| lease.id == id)
            .ok_or(Error::Expired)
    }
    /// Select a side of a comparison without joining identities by spelling.
    /// Both leases must still exist and belong to the same selected project.
    pub fn comparison_pair(
        &mut self,
        base: LeaseId,
        target: LeaseId,
        now: Instant,
    ) -> Result<(&Lease, &Lease), Error> {
        self.expire(now);
        if base.epoch != self.epoch || target.epoch != self.epoch {
            return Err(Error::Expired);
        }
        let base_lease = self
            .slots
            .iter()
            .flatten()
            .find(|lease| lease.id == base)
            .ok_or(Error::Expired)?;
        let target_lease = self
            .slots
            .iter()
            .flatten()
            .find(|lease| lease.id == target)
            .ok_or(Error::Expired)?;
        if base_lease.basis.project != target_lease.basis.project {
            return Err(Error::Invalid);
        }
        Ok((base_lease, target_lease))
    }
    pub fn comparison_side(
        &mut self,
        base: LeaseId,
        target: LeaseId,
        side: ChangeSide,
        now: Instant,
    ) -> Result<&Lease, Error> {
        let (base, target) = self.comparison_pair(base, target, now)?;
        Ok(match side {
            ChangeSide::Base => base,
            ChangeSide::Target => target,
        })
    }
    /// Return both exact selected bases, validating the shared project.
    pub fn comparison_bases(
        &mut self,
        base: LeaseId,
        target: LeaseId,
        now: Instant,
    ) -> Result<(Basis, Basis), Error> {
        let base_basis = self
            .comparison_side(base, target, ChangeSide::Base, now)?
            .basis;
        let target_basis = self
            .comparison_side(base, target, ChangeSide::Target, now)?
            .basis;
        Ok((base_basis, target_basis))
    }
    /// Release an unpublished or explicitly discarded lease.
    pub fn release(&mut self, id: LeaseId) {
        if id.epoch != self.epoch {
            return;
        }
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|lease| lease.id == id))
        {
            if let Some(lease) = slot.take() {
                self.used -= lease.bytes;
            }
        }
    }
    pub fn used(&self) -> usize {
        self.used
    }
}

#[cfg(unix)]
fn fill_epoch(bytes: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(bytes)
}
#[cfg(windows)]
fn fill_epoch(bytes: &mut [u8]) -> std::io::Result<()> {
    backend_platform::win32::random::fill(bytes)
}
#[cfg(not(any(unix, windows)))]
fn fill_epoch(_: &mut [u8]) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "operating-system randomness unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(n: usize) -> Capture {
        Capture {
            basis: Basis {
                manifest: [1; 32],
                native: [2; 32],
                project: [3; 32],
                scope: [4; 32],
                recipe: [5; 32],
                freshness: Freshness::Unknown,
                worker: Worker::Unknown,
            },
            relation_coverage: Coverage::Gapped,
            source_coverage: Coverage::Gapped,
            records: vec![Material {
                key: [1; 32],
                bytes: vec![7; n].into_boxed_slice(),
            }],
            sources: vec![],
        }
    }
    #[test]
    fn retention_is_bounded_expiring_and_owner_scoped() {
        let now = Instant::now();
        let mut store = LeaseStore::new([1; 16]);
        let id = store.insert(capture(12), now, || false).unwrap();
        assert_eq!(store.get(id, now).unwrap().records[0].bytes.len(), 12);
        assert_eq!(
            store.get(id, now).unwrap().source_coverage,
            Coverage::Gapped
        );
        for _ in 0..3 {
            store.insert(capture(1), now, || false).unwrap();
        }
        assert_eq!(
            store.insert(capture(1), now, || false),
            Err(Error::Capacity)
        );
        let restart = LeaseStore::new([2; 16]);
        let mut restart = restart;
        assert!(matches!(restart.get(id, now), Err(Error::Expired)));
        assert!(matches!(store.get(id, now + TTL), Err(Error::Expired)));
        assert_eq!(store.used(), size_of::<LeaseStore>());
    }
    #[test]
    fn comparison_uses_explicit_sides_and_never_opens_missing_sources() {
        let now = Instant::now();
        let mut store = LeaseStore::new([3; 16]);
        let base = store.insert(capture(1), now, || false).unwrap();
        let mut new = capture(1);
        new.basis.native = [9; 32];
        let target = store.insert(new, now, || false).unwrap();
        let removed = store
            .comparison_side(base, target, ChangeSide::Base, now)
            .unwrap();
        assert_eq!(removed.basis.native, [2; 32]);
        assert!(removed.historical_source(&[4; 32]).is_none());
        assert_eq!(
            store
                .comparison_side(base, target, ChangeSide::Target, now)
                .unwrap()
                .basis
                .native,
            [9; 32]
        );
    }
    #[test]
    fn cancellation_and_duplicate_keys_do_not_publish() {
        let now = Instant::now();
        let mut store = LeaseStore::new([1; 16]);
        let before = store.used();
        assert_eq!(
            store.insert(capture(2), now, || true),
            Err(Error::Cancelled)
        );
        let mut duplicate = capture(1);
        duplicate.records.push(Material {
            key: [1; 32],
            bytes: vec![2].into_boxed_slice(),
        });
        assert_eq!(store.insert(duplicate, now, || false), Err(Error::Invalid));
        assert_eq!(store.used(), before);
    }
    #[test]
    fn aggregate_budget_applies_across_leases() {
        let now = Instant::now();
        let mut store = LeaseStore::new([1; 16]);
        store
            .insert(capture(63 * 1024 * 1024), now, || false)
            .unwrap();
        store
            .insert(capture(63 * 1024 * 1024), now, || false)
            .unwrap();
        let before = store.used();
        assert_eq!(
            store.insert(capture(3 * 1024 * 1024), now, || false),
            Err(Error::Capacity)
        );
        assert_eq!(store.used(), before);
        assert!(before <= MAX_WORKSPACE_BYTES);
    }
    #[test]
    fn oversized_capture_is_rejected() {
        let now = Instant::now();
        let mut store = LeaseStore::new([1; 16]);
        assert_eq!(
            store.insert(capture(MAX_LEASE_BYTES), now, || false),
            Err(Error::Capacity)
        );
    }
}
