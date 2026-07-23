//! Overlays = branches/remotes with a merge policy (INDEX-PLAN ID-9).
//!
//! An overlay is a named branch (optionally on a remote endpoint) whose rows are
//! reconciled into `main` under a per-table [`MergePolicy`]. This module holds
//! the **policy types and resolution logic**; the actual remote sync (iroh pull,
//! `dolt_merge`) is out of scope here and lives in the sync plane.
//!
//! The tabular overlay record itself is [`crate::tables::overlays::OverlayRow`];
//! this module adds the typed merge policy that row's `precedence` participates
//! in.

use serde::{Deserialize, Serialize};

/// How a table's rows are reconciled when an overlay branch merges into `main`
/// (INDEX-PLAN ID-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergePolicy {
    /// Upstream (`main`) always wins; the overlay may only *add* rows absent
    /// upstream, never override an upstream row. Used for authoritative registry
    /// facts.
    UpstreamAuthoritative,
    /// The device/overlay owns these rows; the overlay wins on conflict. Used
    /// for `local/<device>` state (e.g. locally-produced generation locations).
    DeviceOwned,
    /// Rows from both sides coexist; a conflict on the same primary key is
    /// resolved by the higher-precedence side. Used for append-mostly tables
    /// (`generation_locations`, `listing_events`, `stores`, `object_locations`).
    Union,
}

impl MergePolicy {
    /// The four tables whose merges the INDEX plan calls out for the `Union`
    /// policy (ID-9). Other tables default to [`MergePolicy::UpstreamAuthoritative`].
    pub const UNION_TABLES: &'static [&'static str] = &[
        crate::tables::locations::GENERATION_LOCATIONS_TABLE,
        crate::tables::locations::OBJECT_LOCATIONS_TABLE,
        crate::tables::stores::TABLE,
        crate::tables::listing::TABLE,
    ];

    /// The default policy for a table name, per ID-9.
    pub fn for_table(table: &str) -> MergePolicy {
        if Self::UNION_TABLES.contains(&table) {
            MergePolicy::Union
        } else {
            MergePolicy::UpstreamAuthoritative
        }
    }
}

/// Which side of a conflict a resolution chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictWinner {
    /// The upstream (`main`) row is kept.
    Upstream,
    /// The overlay row is kept.
    Overlay,
}

/// Resolve a single-key conflict between an upstream row and an overlay row
/// under a policy. `overlay_precedence` is the overlay's `precedence` column
/// (higher wins under [`MergePolicy::Union`]); `upstream_precedence` is `main`'s
/// effective precedence (conventionally `0`).
pub fn resolve_conflict(
    policy: MergePolicy,
    upstream_precedence: i64,
    overlay_precedence: i64,
) -> ConflictWinner {
    match policy {
        MergePolicy::UpstreamAuthoritative => ConflictWinner::Upstream,
        MergePolicy::DeviceOwned => ConflictWinner::Overlay,
        MergePolicy::Union => {
            if overlay_precedence > upstream_precedence {
                ConflictWinner::Overlay
            } else {
                ConflictWinner::Upstream
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_authoritative_always_keeps_upstream() {
        assert_eq!(
            resolve_conflict(MergePolicy::UpstreamAuthoritative, 0, 99),
            ConflictWinner::Upstream
        );
    }

    #[test]
    fn device_owned_always_keeps_overlay() {
        assert_eq!(
            resolve_conflict(MergePolicy::DeviceOwned, 99, 0),
            ConflictWinner::Overlay
        );
    }

    #[test]
    fn union_breaks_ties_by_precedence() {
        assert_eq!(
            resolve_conflict(MergePolicy::Union, 0, 1),
            ConflictWinner::Overlay
        );
        assert_eq!(
            resolve_conflict(MergePolicy::Union, 1, 1),
            ConflictWinner::Upstream
        );
    }

    #[test]
    fn union_tables_get_union_policy() {
        assert_eq!(
            MergePolicy::for_table(crate::tables::stores::TABLE),
            MergePolicy::Union
        );
        assert_eq!(
            MergePolicy::for_table(crate::tables::packages::TABLE),
            MergePolicy::UpstreamAuthoritative
        );
    }
}
