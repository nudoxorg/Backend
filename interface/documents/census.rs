//! Defines census behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the census invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Per-package declaration counts every surface shows on a package card.

use compiler_ir_vocabulary::EntityKind;

/// Slot of one kind inside the per-kind counter array.
///
/// An explicit table rather than a discriminant cast, so the array's layout is this module's
/// decision and stays correct if the vocabulary is ever reordered.
const fn kind_index(kind: EntityKind) -> usize {
    match kind {
        EntityKind::Function => 0,
        EntityKind::Constant => 1,
        EntityKind::Record => 2,
        EntityKind::Module => 3,
        EntityKind::Field => 4,
        EntityKind::Alias => 5,
        EntityKind::Trait => 6,
        EntityKind::Implementation => 7,
        EntityKind::Enum => 8,
        EntityKind::Variant => 9,
        EntityKind::Static => 10,
        EntityKind::Reexport => 11,
        EntityKind::Parameter => 12,
        EntityKind::Macro => 13,
        EntityKind::Namespace => 14,
    }
}

/// One saturating count.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Count(pub u32);

impl Count {
    /// Adds one, saturating at the fixed width.
    #[must_use]
    pub const fn incremented(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Count of one declaration kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KindCount {
    /// Counted kind.
    pub kind: EntityKind,
    /// Declarations of that kind.
    pub count: Count,
}

/// Declaration counts for one package.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Census {
    /// Every canonical declaration.
    pub entities: Count,
    /// Declarations with public visibility.
    pub public: Count,
    /// Declarations with retained documentation.
    pub documented: Count,
    by_kind: [u32; 15],
}

impl Census {
    /// Records one declaration.
    pub fn record(&mut self, kind: EntityKind, public: bool, documented: bool) {
        self.entities = self.entities.incremented();
        if public {
            self.public = self.public.incremented();
        }
        if documented {
            self.documented = self.documented.incremented();
        }
        if let Some(slot) = self.by_kind.get_mut(kind_index(kind)) {
            *slot = slot.saturating_add(1);
        }
    }

    /// Count of one kind.
    #[must_use]
    pub fn of(&self, kind: EntityKind) -> Count {
        match self.by_kind.get(kind_index(kind)) {
            Some(count) => Count(*count),
            None => Count(0),
        }
    }

    /// Non-zero kind counts in canonical kind order.
    pub fn kinds(&self) -> impl Iterator<Item = KindCount> + '_ {
        EntityKind::ALL
            .into_iter()
            .map(|kind| KindCount {
                kind,
                count: self.of(kind),
            })
            .filter(|row| row.count.0 != 0)
    }
}
