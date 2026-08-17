//! Auto-trait facts, `TriState`, and `Sealed` shared vocabulary.
use crate::visitor::Visitor;

// ---------------------------------------------------------------------------
// TriState
// ---------------------------------------------------------------------------

/// A three-valued boolean for facts that may be unknown at record time.
///
/// Used where a two-valued `bool` loses meaning: e.g. object-safety can be
/// confirmed, refuted, or simply not yet analysed.
///
/// The analysis pipeline *must not* collapse `Unknown` to either pole without
/// evidence — treat it as a lattice bottom that is later resolved.
// frozen — never renumber/reorder variants
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub enum TriState {
    /// The predicate provably holds.
    Yes,

    /// The predicate provably does not hold.
    No,

    /// The predicate has not been (or cannot be) determined at record time.
    #[default]
    Unknown,
}

// ---------------------------------------------------------------------------
// Sealed
// ---------------------------------------------------------------------------

/// How thoroughly a trait is sealed against external implementation.
///
/// Produced by sealing-analysis; `None` means no sealing evidence was found,
/// not that the trait is definitively open.
// frozen — never renumber/reorder variants
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize, Default,
)]
pub enum Sealed {
    /// Not sealed — any downstream crate may implement it.
    #[default]
    None,

    /// Sealed via a public supertrait on a private bound (pub-API pattern).
    PubApi,

    /// Fully sealed — only the defining crate can implement it.
    Full,
}

// ---------------------------------------------------------------------------
// AutoTrait, AutoState, AutoFact
// ---------------------------------------------------------------------------

/// Well-known auto traits whose implementation status is worth recording
/// explicitly alongside a type.
///
/// Producers emit these when the oracle (rustdoc, ra, …) exposes the
/// information; downstream consumers may use them for security / correctness
/// reasoning without re-deriving trait bounds.
// frozen — never renumber/reorder variants
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum AutoTrait {
    /// [`std::marker::Send`] — type may be transferred across thread
    /// boundaries.
    Send,

    /// [`std::marker::Sync`] — type may be shared across thread boundaries via
    /// a shared reference.
    Sync,

    /// [`std::marker::Unpin`] — type does not need to be pinned before being
    /// polled.
    Unpin,

    /// [`std::panic::UnwindSafe`] — type is safe to use across an unwind
    /// boundary.
    UnwindSafe,

    /// [`std::panic::RefUnwindSafe`] — type is safe to share across an unwind
    /// boundary via a shared reference.
    RefUnwindSafe,
}

/// Whether a type implements an auto trait.
// frozen — never renumber/reorder variants
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum AutoState {
    /// Unconditionally implements the trait.
    Yes,

    /// Unconditionally does not implement the trait.
    No,

    /// Implements the trait only under certain generic bounds (conditional
    /// impl).
    Cond,
}

/// A single recorded auto-trait fact for a type.
///
/// Bundled as `auto: List<AutoFact>` on [`Record`](crate::kinds::Record) and
/// [`Enum`](crate::kinds::Enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct AutoFact {
    /// Which auto trait this fact describes.
    pub trait_: AutoTrait,

    /// The implementation status for that trait.
    pub state: AutoState,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Serde round-trip for every variant of the auto-trait vocabulary and the
    /// shared TriState / Sealed types (gap 1 / gap 6).
    #[test]
    fn auto_fact_serde_roundtrip() {
        let facts: Vec<AutoFact> = vec![
            AutoFact {
                trait_: AutoTrait::Send,
                state: AutoState::Yes,
            },
            AutoFact {
                trait_: AutoTrait::Sync,
                state: AutoState::No,
            },
            AutoFact {
                trait_: AutoTrait::Unpin,
                state: AutoState::Cond,
            },
            AutoFact {
                trait_: AutoTrait::UnwindSafe,
                state: AutoState::Yes,
            },
            AutoFact {
                trait_: AutoTrait::RefUnwindSafe,
                state: AutoState::No,
            },
        ];

        let json = serde_json::to_string(&facts).expect("serialize failed");
        let rt: Vec<AutoFact> = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(facts, rt);
    }

    #[test]
    fn tristate_default_is_unknown() {
        assert_eq!(TriState::default(), TriState::Unknown);
    }

    #[test]
    fn sealed_default_is_none() {
        assert_eq!(Sealed::default(), Sealed::None);
    }

    /// Serde round-trip for all TriState and Sealed variants.
    #[test]
    fn tristate_sealed_roundtrip() {
        let tristates = [TriState::Yes, TriState::No, TriState::Unknown];
        let sealed = [Sealed::None, Sealed::PubApi, Sealed::Full];

        for ts in tristates {
            let json = serde_json::to_string(&ts).unwrap();
            assert_eq!(ts, serde_json::from_str::<TriState>(&json).unwrap());
        }
        for s in sealed {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(s, serde_json::from_str::<Sealed>(&json).unwrap());
        }
    }
}
