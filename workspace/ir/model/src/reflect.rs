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
// PathStyle
// ---------------------------------------------------------------------------

/// The segment separator an ecosystem's own idiomatic path syntax uses.
///
/// Two disagreeing `path` implementations grew up independently — MCP's
/// compact symbol path (`::`, always) and this module's [`moniker_path`]
/// (`.`, always) — so Java rendered with `::` and Rust rendered with `.`,
/// each wrong for its own ecosystem. `for_ecosystem` is the single decision
/// both producers should defer to, so the mistake can only be made once.
///
/// This type governs *rendering*, never identity: the [`IntroId`] preimage
/// (`bootstrap_intro_id`) encodes ancestor segments individually, with no
/// separator baked in, so choosing a different `PathStyle` to *display* a
/// path never changes what it hashes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// `a::b::c` — Rust (`cargo`), C/C++ (`cpp`).
    DoubleColon,
    /// `a.b.c` — Java (`maven`), Python (`pypi`), npm, NuGet, Go.
    Dot,
}

impl PathStyle {
    /// The idiomatic style for `ecosystem`'s own path syntax.
    ///
    /// Unrecognized ecosystem tags fall back to [`PathStyle::Dot`] — the
    /// majority case among the seven registered ecosystems — rather than
    /// panicking or returning an `Option`: a rendering choice for an unknown
    /// tag should degrade gracefully, not become a new failure mode.
    pub fn for_ecosystem(ecosystem: &str) -> Self {
        match ecosystem {
            "cargo" | "cpp" => PathStyle::DoubleColon,
            _ => PathStyle::Dot,
        }
    }

    /// The literal separator text.
    pub fn separator(self) -> &'static str {
        match self {
            PathStyle::DoubleColon => "::",
            PathStyle::Dot => ".",
        }
    }
}

// ---------------------------------------------------------------------------
// moniker_path
// ---------------------------------------------------------------------------

/// The root-first ancestor chain of `Symbol.name`s from the top ancestor down
/// to `intro`, unjoined (e.g. `["mymod", "MyType", "method"]`).
///
/// Returns `None` if `intro` is absent from `table`. A cycle in the parent
/// chain causes `None` to be returned (defensive; should not happen in a
/// well-formed table).
///
/// Shared by [`moniker_path`] and [`moniker_path_styled`] so the one walk
/// that matters — the exact segment sequence the `IntroId` preimage was
/// hashed from — has exactly one implementation; the two callers differ only
/// in how they join it into a string.
///
/// Public (not just an internal helper) because the address scheme's
/// renderer needs the individual segments, not a pre-joined string, to build
/// an `AddressSegment` list rather than re-splitting a rendered path back
/// apart.
pub fn moniker_segments(table: &PristineIntroTable, intro: IntroId) -> Option<Vec<String>> {
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
    Some(segments)
}

/// The root-first dotted path of `Symbol.name`s from the top ancestor down to
/// `intro` (e.g. `"mymod.MyType.method"`).
///
/// This is the historical, always-`.` rendering used by `Symbol.path` in the
/// GraphQL schema. It is deliberately left unchanged — flipping its separator
/// per-ecosystem is a wire-visible break for anything filtering `path` in
/// `graph_query` and needs filter-side separator normalization to ship first
/// (see the address-scheme design doc §4.9's "ordering hazard"). New callers
/// that want the ecosystem-correct separator should use
/// [`moniker_path_styled`] instead.
///
/// Returns `None` if `intro` is absent from `table`.
pub fn moniker_path(table: &PristineIntroTable, intro: IntroId) -> Option<String> {
    moniker_segments(table, intro).map(|segments| segments.join("."))
}

/// [`moniker_path`], joined with `style`'s separator instead of always `.`.
///
/// This is the *physical path* producer §4.9 calls canonical: it walks the
/// exact same preimage chain [`moniker_path`] does (via [`moniker_segments`]),
/// so the two can never disagree about which ancestors exist — only about how
/// the join is spelled.
pub fn moniker_path_styled(
    table: &PristineIntroTable,
    intro: IntroId,
    style: PathStyle,
) -> Option<String> {
    moniker_segments(table, intro).map(|segments| segments.join(style.separator()))
}

// ---------------------------------------------------------------------------
// monikers
// ---------------------------------------------------------------------------

/// Iterate `(moniker_path, IntroId)` for every **exported** entry in `table`.
///
/// Entries that are not exported (under `policy`) are silently skipped.
/// Entries for which [`moniker_path`] returns `None` (absent ancestors) are
/// also skipped.
pub fn monikers(
    table: &PristineIntroTable,
    policy: ExportPolicy,
) -> impl Iterator<Item = (String, IntroId)> + '_ {
    table.iter().filter_map(move |(intro, _entry)| {
        if !exported(table, policy, intro) {
            return None;
        }
        moniker_path(table, intro).map(|path| (path, intro))
    })
}

/// [`monikers`], joined with `style`'s separator instead of always `.`.
///
/// Exists for the corpus-side cross-package link resolver
/// (`nudox-engine`'s `CorpusResolver`): a [`crate::foreign::ForeignKey::path`]
/// is written in the *target* ecosystem's own spelling — `core::clone::Clone`
/// for cargo, `java.util.List` for maven — so joining a sibling package's
/// monikers with the always-`.` [`monikers`] would fail to match every
/// `::`-styled key from a cargo or cpp producer. This walks the identical
/// exported-entry set [`monikers`] does (same `policy`, same
/// [`exported`] gate) and differs only in the separator, for the same reason
/// [`moniker_path_styled`] differs from [`moniker_path`]: the two must never
/// disagree about *which* ancestors exist, only about how the join is
/// spelled.
pub fn monikers_styled(
    table: &PristineIntroTable,
    policy: ExportPolicy,
    style: PathStyle,
) -> impl Iterator<Item = (String, IntroId)> + '_ {
    table.iter().filter_map(move |(intro, _entry)| {
        if !exported(table, policy, intro) {
            return None;
        }
        moniker_path_styled(table, intro, style).map(|path| (path, intro))
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
        test_helpers::{entry, node},
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
            attrs: Box::new([]),
            cfg: None,
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
                node::root([]),
                Module,
            ),
            None,
        );
        t.insert_live(
            hidden_id,
            entry(
                sym_with_vis("hidden", Visibility::Private),
                node::root([]),
                Module,
            ),
            Some(root_id),
        );
        t.insert_live(
            child_id,
            entry(
                sym_with_vis("child", Visibility::Public),
                node::root([]),
                Module,
            ),
            Some(hidden_id),
        );
        t.insert_live(
            visible_id,
            entry(
                sym_with_vis("visible", Visibility::Public),
                node::root([]),
                Module,
            ),
            Some(root_id),
        );
        t.insert_live(
            leaf_id,
            entry(
                sym_with_vis("leaf", Visibility::Private),
                node::root([]),
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
                node::root([]),
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
                node::root([]),
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
            entry(
                sym_with_vis("pkg", Visibility::Public),
                node::root([]),
                Module,
            ),
            None,
        );
        t.insert_live(
            crate_mod_id,
            entry(
                sym_with_vis("internals", Visibility::Crate),
                node::root([]),
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
