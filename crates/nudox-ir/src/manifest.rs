//! Generation identity — content-addressed stamp for a sealed
//! [`PristineIntroTable`].
//!
//! A [`GenerationStamp`] is a 32-byte BLAKE3 digest that uniquely identifies
//! the *set of IntroIds* present in a generation at the moment it is sealed.
//! It is deterministic and order-independent: two tables with the same set of
//! live intros produce the same stamp regardless of insertion order.

use serde::{Deserialize, Serialize};

use crate::{
    apply::PristineIntroTable,
    change::{encode::write_u32le, hash::ContentBlake3},
};

/// Domain tag for generation stamps.  Changing this string invalidates all
/// previously-computed stamps (by design — bump when the preimage layout
/// changes).
pub const GENERATION_DOMAIN: &str = "nudox.gen.v1";

/// Content-addressed identity of a sealed generation's *intro set*.
///
/// # What this stamps
///
/// The stamp covers the **set of [`crate::change::IntroId`]s** present in the
/// [`PristineIntroTable`] at seal time — i.e. the *shape* of the generation.
///
/// # FIXME — entry-content not yet folded in
///
/// This stamps the *set of IntroIds* (the generation's shape) but NOT
/// per-entry content — a full stamp should fold in each entry's content hash;
/// deferred until the entry-payload-hash class lands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GenerationStamp(ContentBlake3);

impl GenerationStamp {
    /// Wrap a raw `ContentBlake3` (already computed).
    #[inline]
    pub const fn from_raw(inner: ContentBlake3) -> Self {
        Self(inner)
    }

    /// Raw 32 bytes of the digest.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex representation (64 characters).
    #[inline]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for GenerationStamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl core::fmt::Debug for GenerationStamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "gen:{}…", &self.to_hex()[..12])
    }
}

/// Compute the [`GenerationStamp`] for a sealed [`PristineIntroTable`].
///
/// # Algorithm
///
/// 1. Collect all [`crate::change::IntroId`]s from `table.iter()`.
/// 2. Sort them by their 32 raw bytes (stable total order).
/// 3. Build the preimage: `u32le(count)` followed by each intro's 32 raw bytes
///    in sorted order.
/// 4. Return `GenerationStamp(ContentBlake3::from_domain(GENERATION_DOMAIN,
///    &preimage))`.
///
/// This is **deterministic** and **order-independent**: two tables with the
/// same set of live intros always produce the same stamp.
pub fn generation_stamp(table: &PristineIntroTable) -> GenerationStamp {
    // 1. Collect IntroIds.
    let mut intros: Vec<_> = table.iter().map(|(id, _)| id).collect();

    // 2. Sort by raw bytes (IntroId is Ord via ContentBlake3 which is [u8;32]).
    intros.sort();

    // 3. Build the preimage.
    let count = u32::try_from(intros.len()).expect("intro count exceeds u32::MAX");
    let mut preimage = Vec::with_capacity(4 + intros.len() * 32);
    write_u32le(&mut preimage, count);
    for intro in &intros {
        preimage.extend_from_slice(intro.as_bytes());
    }

    // 4. Domain-hash and wrap.
    GenerationStamp(ContentBlake3::from_domain(GENERATION_DOMAIN, &preimage))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        change::IntroId,
        kinds::Module,
        test_helpers::{entry, n, sym},
    };

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    fn make_table(pairs: &[(u8, Option<u8>)]) -> PristineIntroTable {
        let mut t = PristineIntroTable::new();
        for &(id_byte, parent_byte) in pairs {
            let id = intro(id_byte);
            let parent = parent_byte.map(intro);
            t.insert_live(id, entry(sym("x"), n::root([]), Module), parent);
        }
        t
    }

    // (a) Determinism: same content → same stamp.
    #[test]
    fn determinism() {
        let t1 = make_table(&[(1, None), (2, Some(1))]);
        let t2 = make_table(&[(1, None), (2, Some(1))]);
        assert_eq!(
            generation_stamp(&t1),
            generation_stamp(&t2),
            "same table content must yield identical stamps"
        );
    }

    // (b) Order-independence: insertion order must not affect the stamp.
    #[test]
    fn order_independence() {
        // Insert (1, 2) vs (2, 1) — same intros, different insertion order.
        let t_forward = make_table(&[(1, None), (2, Some(1))]);
        let t_reverse = make_table(&[(2, None), (1, Some(2))]);
        assert_eq!(
            generation_stamp(&t_forward),
            generation_stamp(&t_reverse),
            "insertion order must not affect the stamp"
        );
    }

    // (c) Golden pin: exactly IntroId::from_raw([1u8;32]) and
    // IntroId::from_raw([2u8;32]). The expected hex was obtained by running the
    // test and recording the real output.
    #[test]
    fn golden_pin() {
        let mut t = PristineIntroTable::new();
        t.insert_live(
            IntroId::from_raw([1u8; 32]),
            entry(sym("a"), n::root([]), Module),
            None,
        );
        t.insert_live(
            IntroId::from_raw([2u8; 32]),
            entry(sym("b"), n::root([]), Module),
            None,
        );
        let hex = generation_stamp(&t).to_hex();
        assert_eq!(
            hex, "3529873ce7009ee1c6ccd53ede572206c82c984f5dc262f76e2833f15a2d7635",
            "GenerationStamp golden pin changed — preimage layout regression"
        );
    }
}
