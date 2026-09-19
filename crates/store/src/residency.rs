use super::{HashMap, HashSet, Pack, PackId};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// Lease token keeping one admitted pack resident.
pub struct Pin(u64);

impl Pin {
    /// Returns the opaque lease number for diagnostics.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}
#[derive(Default)]
/// In-memory pack residency and pin tracker.
pub struct Residency {
    next: u64,
    pins: HashMap<Pin, PackId>,
    packs: HashMap<PackId, Pack>,
}
impl Residency {
    /// Admits an immutable pack for possible pinning.
    pub fn admit(&mut self, pack: Pack) {
        self.packs.insert(pack.id(), pack);
    }
    /// Pins an admitted pack and returns its lease token.
    pub fn pin(&mut self, id: PackId) -> Option<Pin> {
        if self.packs.contains_key(&id) {
            self.next = self.next.checked_add(1)?;
            let p = Pin(self.next);
            self.pins.insert(p, id);
            Some(p)
        } else {
            None
        }
    }
    /// Releases a previously returned pin token.
    pub fn unpin(&mut self, pin: Pin) {
        self.pins.remove(&pin);
    }
    /// Removes admitted packs with no remaining pins.
    pub fn collect(&mut self) -> usize {
        let live: HashSet<_> = self.pins.values().copied().collect();
        let before = self.packs.len();
        self.packs.retain(|id, _| live.contains(id));
        before - self.packs.len()
    }
    /// Reports whether a pack is currently admitted.
    #[must_use]
    pub fn contains(&self, id: PackId) -> bool {
        self.packs.contains_key(&id)
    }
}
