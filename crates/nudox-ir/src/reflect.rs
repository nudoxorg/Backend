//! Pure read-model reflections over a [`PristineIntroTable`].
//!
//! These are stateless, I/O-free functions that answer two questions given a
//! materialized IR snapshot:
//!
//! 1. **Is this entry exported?** — visibility + ancestry check under a policy.
//! 2. **What is this entry's moniker?** — the root-first dotted name path.
//!
//! There is no `ApiSurface` artifact. "What is the public surface?" is a pure
//! function of the table and a policy; callers can cache the projection.

use std::collections::HashSet;

use crate::{apply::PristineIntroTable, change::IntroId, entry::Visibility};

// ---------------------------------------------------------------------------
// ExportPolicy
// ---------------------------------------------------------------------------

/// The policy deciding which visibilities count as "exported" (part of the
/// public boundary) for a given consumer.
///
/// `PublicOnly` treats only [`Visibility::Public`] as exported.
/// `CrateVisible` widens the boundary to also include `Crate`, `Package`, and
/// `Internal` — the non-private tiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportPolicy {
    /// Only [`Visibility::Public`] symbols are exported.
    PublicOnly,
    /// Public, crate, package, and internal symbols are all exported.
    CrateVisible,
}

impl ExportPolicy {
    /// True if the given visibility is exported under this policy.
    #[inline]
    pub fn permits(self, v: &Visibility) -> bool {
        match self {
            ExportPolicy::PublicOnly => matches!(v, Visibility::Public),
            ExportPolicy::CrateVisible => matches!(
                v,
                Visibility::Public | Visibility::Crate | Visibility::Package | Visibility::Internal
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// exported
// ---------------------------------------------------------------------------

/// True iff the entry at `intro` is exported under `policy`.
///
/// An entry is exported when:
/// * it is present in `table`,
/// * its own visibility is permitted by `policy`, AND
/// * every ancestor up to the root is also exported (recursive).
///
/// A root entry (no parent) is exported iff its own visibility is permitted.
///
/// The walk is bounded by a visited-set to guard against any cycle in the
/// parent chain (which should never occur in a well-formed table, but we defend
/// conservatively — a malformed cycle causes the entry to be treated as NOT
/// exported).
pub fn exported(table: &PristineIntroTable, policy: ExportPolicy, intro: IntroId) -> bool {
    let mut current = Some(intro);
    let mut visited: HashSet<IntroId> = HashSet::new();

    while let Some(id) = current {
        if !visited.insert(id) {
            // Cycle detected — conservatively not exported.
            return false;
        }
        let Some(entry) = table.get(id) else {
            return false; // absent entry → not exported
        };
        if !policy.permits(&entry.sym().visibility) {
            return false;
        }
        current = table.parent_of(id);
    }
    true
}

// ---------------------------------------------------------------------------
// moniker_path
// ---------------------------------------------------------------------------

/// The root-first dotted path of `Symbol.name`s from the top ancestor down to
/// `intro` (e.g. `"mymod.MyType.method"`).
///
/// Returns `None` if `intro` is absent from `table`.
///
/// A cycle in the parent chain causes `None` to be returned (defensive; should
/// not happen in a well-formed table).
pub fn moniker_path(table: &PristineIntroTable, intro: IntroId) -> Option<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut current = Some(intro);
    let mut visited: HashSet<IntroId> = HashSet::new();

    while let Some(id) = current {
        if !visited.insert(id) {
            // Cycle — return None conservatively.
            return None;
        }
        let entry = table.get(id)?;
        segments.push(entry.sym().name.clone());
        current = table.parent_of(id);
    }

    segments.reverse();
    Some(segments.join("."))
}

// ---------------------------------------------------------------------------
// monikers
// ---------------------------------------------------------------------------

/// Iterate `(moniker_path, IntroId)` for every **exported** entry in `table`.
///
/// Entries that are not exported (under `policy`) are silently skipped.
/// Entries for which [`moniker_path`] returns `None` (absent ancestors) are
/// also skipped.
pub fn monikers<'a>(
    table: &'a PristineIntroTable,
    policy: ExportPolicy,
) -> impl Iterator<Item = (String, IntroId)> + 'a {
    table.iter().filter_map(move |(intro, _entry)| {
        if !exported(table, policy, intro) {
            return None;
        }
        moniker_path(table, intro).map(|path| (path, intro))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        entry::{Symbol, Visibility},
        kinds::Module,
        test_helpers::{entry, n},
    };

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    fn sym_with_vis(name: &str, vis: Visibility) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: vis,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
        }
    }

    /// Build a table with:
    ///
    ///   root (Public)
    ///     └─ hidden (Private)
    ///          └─ child (Public)
    ///     └─ visible (Public)
    ///          └─ leaf (Private)
    fn build_table() -> (
        PristineIntroTable,
        IntroId,
        IntroId,
        IntroId,
        IntroId,
        IntroId,
    ) {
        let root_id = intro(1);
        let hidden_id = intro(2);
        let child_id = intro(3);
        let visible_id = intro(4);
        let leaf_id = intro(5);

        let mut t = PristineIntroTable::new();
        t.insert_live(
            root_id,
            entry(
                sym_with_vis("root", Visibility::Public),
                n::root([]),
                Module,
            ),
            None,
        );
        t.insert_live(
            hidden_id,
            entry(
                sym_with_vis("hidden", Visibility::Private),
                n::root([]),
                Module,
            ),
            Some(root_id),
        );
        t.insert_live(
            child_id,
            entry(
                sym_with_vis("child", Visibility::Public),
                n::root([]),
                Module,
            ),
            Some(hidden_id),
        );
        t.insert_live(
            visible_id,
            entry(
                sym_with_vis("visible", Visibility::Public),
                n::root([]),
                Module,
            ),
            Some(root_id),
        );
        t.insert_live(
            leaf_id,
            entry(
                sym_with_vis("leaf", Visibility::Private),
                n::root([]),
                Module,
            ),
            Some(visible_id),
        );

        (t, root_id, hidden_id, child_id, visible_id, leaf_id)
    }

    // --- ExportPolicy::permits ---

    #[test]
    fn policy_public_only_permits_only_public() {
        assert!(ExportPolicy::PublicOnly.permits(&Visibility::Public));
        assert!(!ExportPolicy::PublicOnly.permits(&Visibility::Private));
        assert!(!ExportPolicy::PublicOnly.permits(&Visibility::Protected));
        assert!(!ExportPolicy::PublicOnly.permits(&Visibility::Crate));
        assert!(!ExportPolicy::PublicOnly.permits(&Visibility::Package));
        assert!(!ExportPolicy::PublicOnly.permits(&Visibility::Internal));
    }

    #[test]
    fn policy_crate_visible_permits_non_private_tiers() {
        assert!(ExportPolicy::CrateVisible.permits(&Visibility::Public));
        assert!(ExportPolicy::CrateVisible.permits(&Visibility::Crate));
        assert!(ExportPolicy::CrateVisible.permits(&Visibility::Package));
        assert!(ExportPolicy::CrateVisible.permits(&Visibility::Internal));
        assert!(!ExportPolicy::CrateVisible.permits(&Visibility::Private));
        assert!(!ExportPolicy::CrateVisible.permits(&Visibility::Protected));
    }

    // --- exported: own visibility ---

    #[test]
    fn root_public_is_exported_under_public_only() {
        let (t, root_id, ..) = build_table();
        assert!(exported(&t, ExportPolicy::PublicOnly, root_id));
    }

    #[test]
    fn absent_entry_is_not_exported() {
        let t = PristineIntroTable::new();
        assert!(!exported(&t, ExportPolicy::PublicOnly, intro(99)));
    }

    #[test]
    fn private_root_is_not_exported_under_either_policy() {
        let mut t = PristineIntroTable::new();
        let id = intro(10);
        t.insert_live(
            id,
            entry(
                sym_with_vis("secret", Visibility::Private),
                n::root([]),
                Module,
            ),
            None,
        );
        assert!(!exported(&t, ExportPolicy::PublicOnly, id));
        assert!(!exported(&t, ExportPolicy::CrateVisible, id));
    }

    #[test]
    fn crate_visible_root_is_exported_only_under_crate_visible_policy() {
        let mut t = PristineIntroTable::new();
        let id = intro(11);
        t.insert_live(
            id,
            entry(
                sym_with_vis("internal_mod", Visibility::Crate),
                n::root([]),
                Module,
            ),
            None,
        );
        assert!(!exported(&t, ExportPolicy::PublicOnly, id));
        assert!(exported(&t, ExportPolicy::CrateVisible, id));
    }

    // --- exported: parent chain ---

    #[test]
    fn public_child_of_private_parent_is_not_exported() {
        let (t, _root_id, _hidden_id, child_id, ..) = build_table();
        // child is Public but its parent (hidden) is Private → not exported
        assert!(!exported(&t, ExportPolicy::PublicOnly, child_id));
        assert!(!exported(&t, ExportPolicy::CrateVisible, child_id));
    }

    #[test]
    fn private_child_of_public_parent_is_not_exported() {
        let (t, _root_id, _hidden_id, _child_id, _visible_id, leaf_id) = build_table();
        // leaf is Private → not exported regardless of policy
        assert!(!exported(&t, ExportPolicy::PublicOnly, leaf_id));
        assert!(!exported(&t, ExportPolicy::CrateVisible, leaf_id));
    }

    #[test]
    fn public_child_of_public_parent_is_exported() {
        let (t, _root_id, _hidden_id, _child_id, visible_id, _leaf_id) = build_table();
        assert!(exported(&t, ExportPolicy::PublicOnly, visible_id));
    }

    // --- moniker_path ---

    #[test]
    fn moniker_path_root_is_just_name() {
        let (t, root_id, ..) = build_table();
        assert_eq!(moniker_path(&t, root_id), Some("root".to_string()));
    }

    #[test]
    fn moniker_path_child_is_dotted() {
        let (t, _root_id, _hidden_id, _child_id, visible_id, _leaf_id) = build_table();
        // visible → root.visible
        assert_eq!(
            moniker_path(&t, visible_id),
            Some("root.visible".to_string())
        );
    }

    #[test]
    fn moniker_path_deep_chain() {
        // root → hidden → child gives root.hidden.child even though hidden is private
        // (moniker_path is a pure path function, not gated by policy)
        let (t, _root_id, _hidden_id, child_id, ..) = build_table();
        assert_eq!(
            moniker_path(&t, child_id),
            Some("root.hidden.child".to_string())
        );
    }

    #[test]
    fn moniker_path_absent_returns_none() {
        let t = PristineIntroTable::new();
        assert_eq!(moniker_path(&t, intro(42)), None);
    }

    // --- monikers ---

    #[test]
    fn monikers_yields_only_exported_entries() {
        let (t, _root_id, _hidden_id, _child_id, _visible_id, _leaf_id) = build_table();
        let mut paths: Vec<String> = monikers(&t, ExportPolicy::PublicOnly)
            .map(|(p, _)| p)
            .collect();
        paths.sort();

        // root (Public, no parent) → exported
        // hidden (Private) → not exported
        // child (Public but parent=hidden=Private) → not exported
        // visible (Public, parent=root=Public) → exported
        // leaf (Private) → not exported
        assert_eq!(paths, vec!["root".to_string(), "root.visible".to_string()]);
    }

    #[test]
    fn monikers_crate_visible_widens_boundary() {
        let mut t = PristineIntroTable::new();
        let root_id = intro(20);
        let crate_mod_id = intro(21);
        t.insert_live(
            root_id,
            entry(sym_with_vis("pkg", Visibility::Public), n::root([]), Module),
            None,
        );
        t.insert_live(
            crate_mod_id,
            entry(
                sym_with_vis("internals", Visibility::Crate),
                n::root([]),
                Module,
            ),
            Some(root_id),
        );

        let public_paths: Vec<String> = monikers(&t, ExportPolicy::PublicOnly)
            .map(|(p, _)| p)
            .collect();
        let crate_paths: Vec<String> = monikers(&t, ExportPolicy::CrateVisible)
            .map(|(p, _)| p)
            .collect();

        assert!(!public_paths.iter().any(|p| p.contains("internals")));
        assert!(crate_paths.iter().any(|p| p == "pkg.internals"));
    }

    #[test]
    fn monikers_yields_intro_ids_matching_paths() {
        let (t, root_id, _hidden_id, _child_id, visible_id, _leaf_id) = build_table();
        let pairs: Vec<(String, IntroId)> = monikers(&t, ExportPolicy::PublicOnly).collect();

        // root → root_id, root.visible → visible_id
        assert!(pairs.iter().any(|(p, id)| p == "root" && *id == root_id));
        assert!(
            pairs
                .iter()
                .any(|(p, id)| p == "root.visible" && *id == visible_id)
        );
    }
}
