use super::{HashMap, HashSet, LayoutId, Pack, PackId};

/// Layout-bound evidence that one exact physical pack was admitted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResidentPack {
    id: PackId,
    layout: LayoutId,
}

impl ResidentPack {
    /// Returns the content and layout-bound pack identity.
    #[must_use]
    pub const fn id(self) -> PackId {
        self.id
    }

    /// Returns the admitted physical layout identity.
    #[must_use]
    pub const fn layout(self) -> LayoutId {
        self.layout
    }
}

/// Rejected conflicting admission preserving the incoming immutable pack.
#[derive(Debug)]
pub struct ResidencyConflict {
    existing: ResidentPack,
    incoming: Box<Pack>,
}

impl ResidencyConflict {
    /// Returns the first admitted pack evidence retained by residency.
    #[must_use]
    pub const fn existing(&self) -> ResidentPack {
        self.existing
    }

    /// Returns the conflicting pack without discarding caller ownership.
    #[must_use]
    pub fn into_incoming(self) -> Pack {
        *self.incoming
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// Lease token keeping one admitted pack resident.
pub struct Pin {
    lease: u64,
    resident: ResidentPack,
}

impl Pin {
    /// Returns the opaque lease number for diagnostics.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.lease
    }

    /// Returns the exact pack and layout protected by this pin.
    #[must_use]
    pub const fn resident(self) -> ResidentPack {
        self.resident
    }
}
#[derive(Default)]
/// In-memory pack residency and pin tracker.
pub struct Residency {
    next: u64,
    pins: HashMap<Pin, ResidentPack>,
    packs: HashMap<PackId, Pack>,
}
impl Residency {
    /// Admits an immutable pack for possible pinning.
    ///
    /// # Errors
    ///
    /// Returns the incoming pack intact if its identity conflicts with the
    /// first admitted physical pack.
    pub fn admit(&mut self, pack: Pack) -> Result<ResidentPack, ResidencyConflict> {
        let resident = ResidentPack {
            id: pack.id(),
            layout: pack.layout(),
        };
        if let Some(existing) = self.packs.get(&resident.id) {
            let admitted = ResidentPack {
                id: existing.id(),
                layout: existing.layout(),
            };
            if existing == &pack {
                return Ok(admitted);
            }
            return Err(ResidencyConflict {
                existing: admitted,
                incoming: Box::new(pack),
            });
        }
        self.packs.insert(resident.id, pack);
        Ok(resident)
    }
    /// Pins an admitted pack and returns its lease token.
    pub fn pin(&mut self, id: PackId) -> Option<Pin> {
        let pack = self.packs.get(&id)?;
        let resident = ResidentPack {
            id,
            layout: pack.layout(),
        };
        self.pin_resident(resident)
    }

    /// Pins the exact admitted layout named by resident evidence.
    pub fn pin_resident(&mut self, resident: ResidentPack) -> Option<Pin> {
        let pack = self.packs.get(&resident.id)?;
        if pack.layout() != resident.layout {
            return None;
        }
        self.next = self.next.checked_add(1)?;
        let pin = Pin {
            lease: self.next,
            resident,
        };
        self.pins.insert(pin, resident);
        Some(pin)
    }
    /// Releases a previously returned pin token.
    pub fn unpin(&mut self, pin: Pin) {
        self.pins.remove(&pin);
    }
    /// Removes admitted packs with no remaining pins.
    pub fn collect(&mut self) -> usize {
        let live: HashSet<_> = self.pins.values().map(|resident| resident.id).collect();
        let before = self.packs.len();
        self.packs.retain(|id, _| live.contains(id));
        before - self.packs.len()
    }
    /// Reports whether a pack is currently admitted.
    #[must_use]
    pub fn contains(&self, id: PackId) -> bool {
        self.packs.contains_key(&id)
    }

    /// Borrows the exact immutable pack protected by a live pin.
    #[must_use]
    pub fn pinned(&self, pin: Pin) -> Option<&Pack> {
        let resident = self.pins.get(&pin)?;
        let pack = self.packs.get(&resident.id)?;
        (pack.layout() == resident.layout).then_some(pack)
    }
}
